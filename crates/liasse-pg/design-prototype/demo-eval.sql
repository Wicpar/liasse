-- Feasibility demo for the GENERAL evaluator (read-side pushdown): one query
-- serves a `$view` — filter (boolean face), projection incl. a computed-style
-- output (jsonb face), and `$sort` with §7.3 none placement (order-preserving
-- bytea face) — index-served, verdict-identical to a native-SQL oracle.
-- Run AFTER demo.sql (reuses its `nodes` table and companies tree).

-- ---- seed: a top-level `projects` collection under the root sentinel -------
-- Fields (tagged wire): name text, status text, revenue int, cost int,
-- priority int — ABSENT (none) for every k % 5 = 0, so sort-none placement is
-- exercised. status 'closed' for k % 7 = 0, so the filter prunes.
INSERT INTO nodes (parent_id, step_name, key_enc, key_wire, value)
SELECT 0, 'projects', int4send(k), jsonb_build_object('i', k),
       jsonb_build_object('st',
         jsonb_build_array(
           jsonb_build_array('name',    jsonb_build_object('s', 'p' || lpad(k::text, 4, '0'))),
           jsonb_build_array('status',  jsonb_build_object('s', CASE WHEN k % 7 = 0 THEN 'closed' ELSE 'open' END)),
           jsonb_build_array('revenue', jsonb_build_object('i', (k * 37) % 1000)),
           jsonb_build_array('cost',    jsonb_build_object('i', (k * 11) % 500)))
         || CASE WHEN k % 5 = 0 THEN '[]'::jsonb
            ELSE jsonb_build_array(jsonb_build_array('priority', jsonb_build_object('i', k % 13))) END)
FROM generate_series(1, 2000) AS k;

-- A second declared nested collection under company rows, so the
-- shape-directed multi-collection descent has two step_names to walk.
INSERT INTO nodes (parent_id, step_name, key_enc, value)
SELECT p.id, 'offices', int4send(k), '{"st": [["kind", {"s": "office"}]]}'
FROM nodes p, generate_series(1, 2) AS k
WHERE p.step_name IN ('companies', 'subcompanies') AND p.depth <= 2;

ANALYZE nodes;

-- ---- the pushed-down $view: ONE query, three evaluator faces ---------------
-- view: .projects[:p | p.status != 'closed'] { name, margin: revenue - cost }
--       $sort: [-priority, name]
\echo '=== $view via the general evaluator: first 5 rows ==='
SELECT liasse_eval_demo_project(
         convert_to('{"outputs":[{"name":"name","field":"name"},{"name":"margin","sub":["revenue","cost"]}]}', 'UTF8'),
         c.value) AS row,
       c.key_wire AS key
FROM nodes c
WHERE c.parent_id = 0 AND c.step_name = 'projects' AND c.value IS NOT NULL
  AND liasse_eval_demo(convert_to('{"field":"status","ne":"closed"}', 'UTF8'), c.value) IS TRUE
ORDER BY liasse_eval_demo_ord(
           convert_to('{"keys":[{"field":"priority","desc":true},{"field":"name","desc":false}]}', 'UTF8'),
           c.value),
         c.key_enc
LIMIT 5;

-- ---- oracle: the same view hand-written in native SQL ----------------------
-- Same filter, projection, and §7.3 order (desc => none first: NULLS FIRST;
-- asc => none last), same occurrence tiebreak (key_enc). Full-result parity.
\echo '=== parity: extension row stream = native-SQL oracle row stream ==='
WITH fields AS (
    SELECT c.key_enc, c.key_wire,
           (SELECT q.pair->1->>'s' FROM jsonb_array_elements(c.value->'st') q(pair) WHERE q.pair->>0 = 'name')     AS name,
           (SELECT q.pair->1->>'s' FROM jsonb_array_elements(c.value->'st') q(pair) WHERE q.pair->>0 = 'status')   AS status,
           (SELECT (q.pair->1->>'i')::bigint FROM jsonb_array_elements(c.value->'st') q(pair) WHERE q.pair->>0 = 'priority') AS priority,
           (SELECT (q.pair->1->>'i')::bigint FROM jsonb_array_elements(c.value->'st') q(pair) WHERE q.pair->>0 = 'revenue')  AS revenue,
           (SELECT (q.pair->1->>'i')::bigint FROM jsonb_array_elements(c.value->'st') q(pair) WHERE q.pair->>0 = 'cost')     AS cost
    FROM nodes c
    WHERE c.parent_id = 0 AND c.step_name = 'projects' AND c.value IS NOT NULL
),
oracle AS (
    SELECT jsonb_build_object(
             'name',   jsonb_build_object('s', name),
             'margin', CASE WHEN revenue IS NULL OR cost IS NULL THEN '{"none":true}'::jsonb
                            ELSE jsonb_build_object('i', revenue - cost) END) AS row
    FROM fields
    WHERE status IS DISTINCT FROM 'closed'
    ORDER BY priority DESC NULLS FIRST, name ASC, key_enc
),
ext AS (
    SELECT liasse_eval_demo_project(
             convert_to('{"outputs":[{"name":"name","field":"name"},{"name":"margin","sub":["revenue","cost"]}]}', 'UTF8'),
             c.value) AS row
    FROM nodes c
    WHERE c.parent_id = 0 AND c.step_name = 'projects' AND c.value IS NOT NULL
      AND liasse_eval_demo(convert_to('{"field":"status","ne":"closed"}', 'UTF8'), c.value) IS TRUE
    ORDER BY liasse_eval_demo_ord(
               convert_to('{"keys":[{"field":"priority","desc":true},{"field":"name","desc":false}]}', 'UTF8'),
               c.value),
             c.key_enc
)
SELECT (SELECT jsonb_agg(row) FROM ext) = (SELECT jsonb_agg(row) FROM oracle) AS parity,
       (SELECT count(*) FROM ext) AS view_rows,
       (SELECT count(*) FROM fields) AS stored_rows;

-- ---- plan: index-served scan, eval only in Filter / Sort Key / target ------
\echo '=== plan: pushed-down $view (no Seq Scan; eval in Filter+SortKey only) ==='
EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF)
SELECT liasse_eval_demo_project(
         convert_to('{"outputs":[{"name":"name","field":"name"},{"name":"margin","sub":["revenue","cost"]}]}', 'UTF8'),
         c.value) AS row,
       c.key_wire AS key
FROM nodes c
WHERE c.parent_id = 0 AND c.step_name = 'projects' AND c.value IS NOT NULL
  AND liasse_eval_demo(convert_to('{"field":"status","ne":"closed"}', 'UTF8'), c.value) IS TRUE
ORDER BY liasse_eval_demo_ord(
           convert_to('{"keys":[{"field":"priority","desc":true},{"field":"name","desc":false}]}', 'UTF8'),
           c.value),
         c.key_enc;

\echo '=== plan: same view with $limit 10 (top-N heapsort, bounded) ==='
EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF)
SELECT c.key_wire
FROM nodes c
WHERE c.parent_id = 0 AND c.step_name = 'projects' AND c.value IS NOT NULL
  AND liasse_eval_demo(convert_to('{"field":"status","ne":"closed"}', 'UTF8'), c.value) IS TRUE
ORDER BY liasse_eval_demo_ord(
           convert_to('{"keys":[{"field":"priority","desc":true},{"field":"name","desc":false}]}', 'UTF8'),
           c.value),
         c.key_enc
LIMIT 10;

-- ---- shape-directed multi-collection descent -------------------------------
-- The recursive term names the DECLARED nested step_names (`= ANY`), so every
-- probe is (parent_id, step_name) against node_key_lookup. PostgreSQL allows
-- exactly one self-reference in the recursive term, so K declared child
-- collections descend through one term with a K-element ANY array.
\echo '=== plan: shape-directed descent over TWO declared collections ==='
EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF)
WITH RECURSIVE tree AS (
    SELECT n.id FROM nodes n
    WHERE n.parent_id = 0 AND n.step_name = 'companies'
      AND n.key_enc = '\x0001'::bytea AND n.value IS NOT NULL
  UNION ALL
    SELECT c.id FROM tree p
    JOIN nodes c ON c.parent_id = p.id
                AND c.step_name = ANY ('{subcompanies,offices}'::text[])
    WHERE c.value IS NOT NULL
)
SELECT count(*) FROM tree;

-- ---- the all-children trap (the Phase-5 finding), pinned -------------------
\echo '=== plan: all-children descent (no step_name) — the Seq Scan trap ==='
EXPLAIN (COSTS OFF)
WITH RECURSIVE tree AS (
    SELECT n.id FROM nodes n
    WHERE n.parent_id = 0 AND n.step_name = 'companies'
      AND n.key_enc = '\x0001'::bytea
  UNION ALL
    SELECT c.id FROM tree p JOIN nodes c ON c.parent_id = p.id
)
SELECT count(*) FROM tree;

\echo '=== plan: all-children descent WITH a dedicated parent_id index ==='
CREATE INDEX nodes_parent ON nodes (parent_id);
ANALYZE nodes;
EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF)
WITH RECURSIVE tree AS (
    SELECT n.id FROM nodes n
    WHERE n.parent_id = 0 AND n.step_name = 'companies'
      AND n.key_enc = '\x0001'::bytea
  UNION ALL
    SELECT c.id FROM tree p JOIN nodes c ON c.parent_id = p.id
)
SELECT count(*) FROM tree;
DROP INDEX nodes_parent;
