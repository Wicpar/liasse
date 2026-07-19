-- Feasibility demo: the §10.5 coverage CTE pruning during descent through the
-- pgrx extension function, on the v4-shaped nodes DDL, index-served.

CREATE TABLE nodes (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    parent_id bigint NOT NULL,
    step_name text NOT NULL,
    key_enc bytea NOT NULL,
    key_wire jsonb NOT NULL DEFAULT '{}',
    incarnation text NOT NULL DEFAULT '',
    value jsonb,
    depth int NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX node_key_lookup ON nodes (parent_id, step_name, key_enc);

-- Root sentinel (id = 0, self-parented), as in the real schema.
INSERT INTO nodes (id, parent_id, step_name, key_enc, value)
OVERRIDING SYSTEM VALUE VALUES (0, 0, '', '\x'::bytea, '{}');

-- Covered root: /companies['1'], status open.
INSERT INTO nodes (parent_id, step_name, key_enc, value, depth)
VALUES (0, 'companies', '\x0001'::bytea,
        '{"st": [["status", {"s": "open"}]]}', 0);

-- Four levels of `subcompanies`, fanout 5; roughly a third of the nodes are
-- 'closed'. Children are inserted under closed nodes too, so pruning has
-- subtrees to skip.
DO $$
DECLARE lvl int;
BEGIN
  FOR lvl IN 1..4 LOOP
    INSERT INTO nodes (parent_id, step_name, key_enc, value, depth)
    SELECT p.id, 'subcompanies', int4send(k),
           jsonb_build_object('st', jsonb_build_array(
             jsonb_build_array('status', jsonb_build_object('s',
               CASE WHEN (p.id + k) % 3 = 0 THEN 'closed' ELSE 'open' END)))),
           lvl
    FROM nodes p, generate_series(1, 5) AS k
    WHERE p.depth = lvl - 1 AND p.id > 0
      AND p.step_name IN ('companies', 'subcompanies');
  END LOOP;
END $$;

-- Noise rows so the planner has a reason to pick the index.
INSERT INTO nodes (parent_id, step_name, key_enc, value)
SELECT 0, 'noise', int4send(k), '{"st": []}'
FROM generate_series(1, 40000) AS k;

ANALYZE nodes;

-- The compiled coverage CTE: anchor unfiltered, recursive term pruned during
-- descent by the extension function (tombstone barrier kept).
\echo '=== coverage via extension (count) ==='
WITH RECURSIVE cover AS (
    SELECT n.id, ARRAY[]::bytea[] AS sort_path, n.value
    FROM nodes n
    WHERE n.parent_id = 0 AND n.step_name = 'companies'
      AND n.key_enc = '\x0001'::bytea AND n.value IS NOT NULL
  UNION ALL
    SELECT c.id, p.sort_path || c.key_enc, c.value
    FROM cover p
    JOIN nodes c ON c.parent_id = p.id AND c.step_name = 'subcompanies'
    WHERE c.value IS NOT NULL
      AND liasse_eval_demo(
            convert_to('{"field":"status","ne":"closed"}', 'UTF8'),
            c.value) IS TRUE
)
SELECT count(*) AS included FROM cover;

-- Reference recursion with the predicate hand-written in native SQL: the
-- oracle count the extension path must match.
\echo '=== coverage via native-SQL predicate (oracle count) ==='
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
                    FROM jsonb_array_elements(c.value->'st') AS q(pair)
                    WHERE q.pair->>0 = 'status'), '') IS DISTINCT FROM 'closed'
)
SELECT count(*) AS included FROM cover;

\echo '=== total stored subtree size (what pruning avoids fetching) ==='
WITH RECURSIVE sub AS (
    SELECT n.id FROM nodes n
    WHERE n.parent_id = 0 AND n.step_name = 'companies'
      AND n.key_enc = '\x0001'::bytea
  UNION ALL
    SELECT c.id FROM sub p
    JOIN nodes c ON c.parent_id = p.id AND c.step_name = 'subcompanies'
)
SELECT count(*) AS stored FROM sub;

\echo '=== plan: extension call as per-row filter, index-served ==='
EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF)
WITH RECURSIVE cover AS (
    SELECT n.id, ARRAY[]::bytea[] AS sort_path, n.value
    FROM nodes n
    WHERE n.parent_id = 0 AND n.step_name = 'companies'
      AND n.key_enc = '\x0001'::bytea AND n.value IS NOT NULL
  UNION ALL
    SELECT c.id, p.sort_path || c.key_enc, c.value
    FROM cover p
    JOIN nodes c ON c.parent_id = p.id AND c.step_name = 'subcompanies'
    WHERE c.value IS NOT NULL
      AND liasse_eval_demo(
            convert_to('{"field":"status","ne":"closed"}', 'UTF8'),
            c.value) IS TRUE
)
SELECT sort_path, value FROM cover ORDER BY sort_path;

\echo '=== abi handshake ==='
SELECT liasse_demo_abi();
