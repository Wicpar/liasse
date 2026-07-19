-- Feasibility demo for CANDIDATE-SUBTREE READS (v5, option (a)): a predicate
-- that reads THROUGH the candidate's own nested collections is served by
-- prefetching the candidate's live subtree in an index-served recursive
-- LATERAL and passing the aggregated rows to the evaluator as one more
-- IMMUTABLE argument — the evaluator never reads tables, the plan stays
-- index-served, and a cyclic/corrupt descent bails with an error, never hangs.
-- Run AFTER demo.sql (reuses its `nodes` table and companies tree).

-- Predicate under test (stand-in for e.g.
--   `child.status != 'closed' && size(child.subcompanies…[open]) >= 100`
-- composed through the evaluator): candidate open AND >= N open rows in the
-- candidate's whole live `subcompanies` subtree.

-- ---- the flat candidate read: children of /companies['1'] ------------------
\echo '=== subtree-reading predicate via lateral prefetch (per-candidate) ==='
SELECT c.key_enc,
       liasse_eval_demo_subtree(
         convert_to('{"open_at_least":100}', 'UTF8'), c.value, st.subtree) AS admitted
FROM nodes c
CROSS JOIN LATERAL (
    WITH RECURSIVE sub AS (
        SELECT s.id, s.step_name, s.key_wire, s.value, 1 AS depth
        FROM nodes s
        WHERE s.parent_id = c.id AND s.step_name = ANY ('{subcompanies}'::text[])
          AND s.value IS NOT NULL              -- live rows only: tombstone blocks
      UNION ALL
        SELECT t.id, t.step_name, t.key_wire, t.value, sub.depth + 1
        FROM sub JOIN nodes t ON t.parent_id = sub.id
                             AND t.step_name = ANY ('{subcompanies}'::text[])
        WHERE t.value IS NOT NULL
          AND liasse_demo_guard(sub.depth + 1, 64)   -- cycle/depth bail, loud
    )
    SELECT COALESCE(jsonb_agg(jsonb_build_array(sub.step_name, sub.key_wire, sub.value)),
                    '[]'::jsonb) AS subtree
    FROM sub
) st
WHERE c.parent_id = (SELECT a.id FROM nodes a
                     WHERE a.parent_id = 0 AND a.step_name = 'companies'
                       AND a.key_enc = '\x0001'::bytea)
  AND c.step_name = 'subcompanies' AND c.value IS NOT NULL
ORDER BY c.key_enc;

-- ---- oracle: the same verdicts hand-written in native SQL ------------------
\echo '=== parity: extension verdicts = native-SQL oracle verdicts ==='
WITH cands AS (
    SELECT c.id, c.key_enc, c.value
    FROM nodes c
    WHERE c.parent_id = (SELECT a.id FROM nodes a
                         WHERE a.parent_id = 0 AND a.step_name = 'companies'
                           AND a.key_enc = '\x0001'::bytea)
      AND c.step_name = 'subcompanies' AND c.value IS NOT NULL
),
oracle AS (
    SELECT cands.key_enc,
           (COALESCE((SELECT q.pair->1->>'s'
                      FROM jsonb_array_elements(cands.value->'st') q(pair)
                      WHERE q.pair->>0 = 'status'), '') IS DISTINCT FROM 'closed')
           AND
           (WITH RECURSIVE sub AS (
                SELECT s.id, s.value FROM nodes s
                WHERE s.parent_id = cands.id AND s.step_name = 'subcompanies'
                  AND s.value IS NOT NULL
              UNION ALL
                SELECT t.id, t.value FROM sub
                JOIN nodes t ON t.parent_id = sub.id AND t.step_name = 'subcompanies'
                WHERE t.value IS NOT NULL
            )
            SELECT count(*) FROM sub
            WHERE COALESCE((SELECT q.pair->1->>'s'
                            FROM jsonb_array_elements(sub.value->'st') q(pair)
                            WHERE q.pair->>0 = 'status'), '') IS DISTINCT FROM 'closed'
           ) >= 100 AS admitted
    FROM cands
),
ext AS (
    SELECT c.key_enc,
           liasse_eval_demo_subtree(
             convert_to('{"open_at_least":100}', 'UTF8'), c.value, st.subtree) AS admitted
    FROM nodes c
    CROSS JOIN LATERAL (
        WITH RECURSIVE sub AS (
            SELECT s.id, s.step_name, s.key_wire, s.value, 1 AS depth
            FROM nodes s
            WHERE s.parent_id = c.id AND s.step_name = ANY ('{subcompanies}'::text[])
              AND s.value IS NOT NULL
          UNION ALL
            SELECT t.id, t.step_name, t.key_wire, t.value, sub.depth + 1
            FROM sub JOIN nodes t ON t.parent_id = sub.id
                                 AND t.step_name = ANY ('{subcompanies}'::text[])
            WHERE t.value IS NOT NULL AND liasse_demo_guard(sub.depth + 1, 64)
        )
        SELECT COALESCE(jsonb_agg(jsonb_build_array(sub.step_name, sub.key_wire, sub.value)),
                        '[]'::jsonb) AS subtree
        FROM sub
    ) st
    WHERE c.parent_id = (SELECT a.id FROM nodes a
                         WHERE a.parent_id = 0 AND a.step_name = 'companies'
                           AND a.key_enc = '\x0001'::bytea)
      AND c.step_name = 'subcompanies' AND c.value IS NOT NULL
)
SELECT (SELECT jsonb_agg(jsonb_build_array(key_enc::text, admitted) ORDER BY key_enc) FROM ext)
     = (SELECT jsonb_agg(jsonb_build_array(key_enc::text, admitted) ORDER BY key_enc) FROM oracle)
       AS parity,
       (SELECT count(*) FILTER (WHERE admitted) FROM ext) AS admitted,
       (SELECT count(*) FROM ext) AS candidates;

-- ---- plan: candidate scan + per-candidate subtree lateral, all index-served
\echo '=== plan: subtree lateral (no Seq Scan; eval face in Filter only) ==='
EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF)
SELECT c.key_enc
FROM nodes c
CROSS JOIN LATERAL (
    WITH RECURSIVE sub AS (
        SELECT s.id, s.step_name, s.key_wire, s.value, 1 AS depth
        FROM nodes s
        WHERE s.parent_id = c.id AND s.step_name = ANY ('{subcompanies}'::text[])
          AND s.value IS NOT NULL
      UNION ALL
        SELECT t.id, t.step_name, t.key_wire, t.value, sub.depth + 1
        FROM sub JOIN nodes t ON t.parent_id = sub.id
                             AND t.step_name = ANY ('{subcompanies}'::text[])
        WHERE t.value IS NOT NULL AND liasse_demo_guard(sub.depth + 1, 64)
    )
    SELECT COALESCE(jsonb_agg(jsonb_build_array(sub.step_name, sub.key_wire, sub.value)),
                    '[]'::jsonb) AS subtree
    FROM sub
) st
WHERE c.parent_id = (SELECT a.id FROM nodes a
                     WHERE a.parent_id = 0 AND a.step_name = 'companies'
                       AND a.key_enc = '\x0001'::bytea)
  AND c.step_name = 'subcompanies' AND c.value IS NOT NULL
  AND liasse_eval_demo_subtree(
        convert_to('{"open_at_least":100}', 'UTF8'), c.value, st.subtree) IS TRUE;

-- ---- the same mechanism inside the §10.5 coverage CTE ----------------------
-- Admit = candidate open AND >= 2 open rows in the candidate's own subtree:
-- the lateral rides in the RECURSIVE TERM (legal: the self-reference appears
-- once; the lateral references only `nodes`), so hereditary pruning and the
-- subtree read compose.
\echo '=== coverage CTE with subtree-reading admit (count) ==='
WITH RECURSIVE cover AS (
    SELECT n.id, ARRAY[]::bytea[] AS sort_path
    FROM nodes n
    WHERE n.parent_id = 0 AND n.step_name = 'companies'
      AND n.key_enc = '\x0001'::bytea AND n.value IS NOT NULL
  UNION ALL
    SELECT c.id, p.sort_path || c.key_enc
    FROM cover p
    JOIN nodes c ON c.parent_id = p.id AND c.step_name = 'subcompanies'
    CROSS JOIN LATERAL (
        WITH RECURSIVE sub AS (
            SELECT s.id, s.step_name, s.key_wire, s.value, 1 AS depth
            FROM nodes s
            WHERE s.parent_id = c.id AND s.step_name = ANY ('{subcompanies}'::text[])
              AND s.value IS NOT NULL
          UNION ALL
            SELECT t.id, t.step_name, t.key_wire, t.value, sub.depth + 1
            FROM sub JOIN nodes t ON t.parent_id = sub.id
                                 AND t.step_name = ANY ('{subcompanies}'::text[])
            WHERE t.value IS NOT NULL AND liasse_demo_guard(sub.depth + 1, 64)
        )
        SELECT COALESCE(jsonb_agg(jsonb_build_array(sub.step_name, sub.key_wire, sub.value)),
                        '[]'::jsonb) AS subtree
        FROM sub
    ) st
    WHERE c.value IS NOT NULL
      AND liasse_eval_demo_subtree(
            convert_to('{"open_at_least":2}', 'UTF8'), c.value, st.subtree) IS TRUE
)
SELECT count(*) AS included FROM cover;

\echo '=== coverage oracle: same admit hand-written in native SQL (count) ==='
WITH RECURSIVE cover AS (
    SELECT n.id FROM nodes n
    WHERE n.parent_id = 0 AND n.step_name = 'companies'
      AND n.key_enc = '\x0001'::bytea AND n.value IS NOT NULL
  UNION ALL
    SELECT c.id
    FROM cover p
    JOIN nodes c ON c.parent_id = p.id AND c.step_name = 'subcompanies'
    WHERE c.value IS NOT NULL
      AND COALESCE((SELECT q.pair->1->>'s'
                    FROM jsonb_array_elements(c.value->'st') q(pair)
                    WHERE q.pair->>0 = 'status'), '') IS DISTINCT FROM 'closed'
      AND (WITH RECURSIVE sub AS (
               SELECT s.id, s.value FROM nodes s
               WHERE s.parent_id = c.id AND s.step_name = 'subcompanies'
                 AND s.value IS NOT NULL
             UNION ALL
               SELECT t.id, t.value FROM sub
               JOIN nodes t ON t.parent_id = sub.id AND t.step_name = 'subcompanies'
               WHERE t.value IS NOT NULL
           )
           SELECT count(*) FROM sub
           WHERE COALESCE((SELECT q.pair->1->>'s'
                           FROM jsonb_array_elements(sub.value->'st') q(pair)
                           WHERE q.pair->>0 = 'status'), '') IS DISTINCT FROM 'closed'
          ) >= 2
)
SELECT count(*) AS included FROM cover;

\echo '=== plan: coverage CTE with the recursive-term lateral ==='
EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF)
WITH RECURSIVE cover AS (
    SELECT n.id, ARRAY[]::bytea[] AS sort_path
    FROM nodes n
    WHERE n.parent_id = 0 AND n.step_name = 'companies'
      AND n.key_enc = '\x0001'::bytea AND n.value IS NOT NULL
  UNION ALL
    SELECT c.id, p.sort_path || c.key_enc
    FROM cover p
    JOIN nodes c ON c.parent_id = p.id AND c.step_name = 'subcompanies'
    CROSS JOIN LATERAL (
        WITH RECURSIVE sub AS (
            SELECT s.id, s.step_name, s.key_wire, s.value, 1 AS depth
            FROM nodes s
            WHERE s.parent_id = c.id AND s.step_name = ANY ('{subcompanies}'::text[])
              AND s.value IS NOT NULL
          UNION ALL
            SELECT t.id, t.step_name, t.key_wire, t.value, sub.depth + 1
            FROM sub JOIN nodes t ON t.parent_id = sub.id
                                 AND t.step_name = ANY ('{subcompanies}'::text[])
            WHERE t.value IS NOT NULL AND liasse_demo_guard(sub.depth + 1, 64)
        )
        SELECT COALESCE(jsonb_agg(jsonb_build_array(sub.step_name, sub.key_wire, sub.value)),
                        '[]'::jsonb) AS subtree
        FROM sub
    ) st
    WHERE c.value IS NOT NULL
      AND liasse_eval_demo_subtree(
            convert_to('{"open_at_least":2}', 'UTF8'), c.value, st.subtree) IS TRUE
)
SELECT sort_path FROM cover ORDER BY sort_path;

-- ---- cyclic data bails with an error, never hangs --------------------------
-- Under the single-parent adjacency schema a parent cycle is a DETACHED ring
-- (every ring member's parent is a ring member, so no address-resolved
-- descent can enter it). Build one, prove (a) sound reads are unaffected,
-- and (b) a descent forced to start INSIDE the ring — the worst corrupt
-- case — errors at the guard bound instead of spinning forever.
CREATE TEMP TABLE ring(r1 bigint, r2 bigint);
WITH a AS (
    INSERT INTO nodes (parent_id, step_name, key_enc, value)
    VALUES (0, 'subcompanies', '\x00f1'::bytea, '{"st": []}') RETURNING id
), b AS (
    INSERT INTO nodes (parent_id, step_name, key_enc, value)
    SELECT id, 'subcompanies', '\x00f2'::bytea, '{"st": []}' FROM a RETURNING id
)
INSERT INTO ring SELECT (SELECT id FROM a), (SELECT id FROM b);
UPDATE nodes SET parent_id = (SELECT r2 FROM ring) WHERE id = (SELECT r1 FROM ring);

\echo '=== sound read with the ring present: unaffected (ring is unreachable) ==='
WITH RECURSIVE sub AS (
    SELECT s.id, 1 AS depth FROM nodes s
    WHERE s.parent_id = (SELECT a.id FROM nodes a
                         WHERE a.parent_id = 0 AND a.step_name = 'companies'
                           AND a.key_enc = '\x0001'::bytea)
      AND s.step_name = ANY ('{subcompanies}'::text[]) AND s.value IS NOT NULL
  UNION ALL
    SELECT t.id, sub.depth + 1 FROM sub
    JOIN nodes t ON t.parent_id = sub.id AND t.step_name = ANY ('{subcompanies}'::text[])
    WHERE t.value IS NOT NULL AND liasse_demo_guard(sub.depth + 1, 64)
)
SELECT count(*) AS live_subtree_rows FROM sub;

\echo '=== descent anchored INSIDE the ring: expect a guard ERROR, no hang ==='
WITH RECURSIVE sub AS (
    SELECT s.id, 1 AS depth FROM nodes s
    WHERE s.parent_id = (SELECT r1 FROM ring)
      AND s.step_name = ANY ('{subcompanies}'::text[]) AND s.value IS NOT NULL
  UNION ALL
    SELECT t.id, sub.depth + 1 FROM sub
    JOIN nodes t ON t.parent_id = sub.id AND t.step_name = ANY ('{subcompanies}'::text[])
    WHERE t.value IS NOT NULL AND liasse_demo_guard(sub.depth + 1, 64)
)
SELECT count(*) FROM sub;

\echo '=== cleanup ring ==='
DELETE FROM nodes WHERE id IN (SELECT r1 FROM ring UNION SELECT r2 FROM ring);
DROP TABLE ring;
