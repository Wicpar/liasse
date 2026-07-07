# Resolutions

Every question from the v0.2 OPEN-QUESTIONS file, resolved. The guiding
rule throughout: derive the answer from machinery that already exists before
adding any.

## 1. CEL profile — resolved

Profile v1, versioned by `$liasse: 1`:

```text
logic       == != < <= > >= && || ! ?: in has(x)
text        size trim lower upper startsWith endsWith contains
            replace split join matches text(x)
int         + - * div mod abs min max sign int(text) trunc(decimal)
            (/ is not defined on int; use div, or convert to decimal)
decimal     + - * / round(d, scale) scale(d) abs min max sign
            decimal(int) decimal(text)   (round is half-even)
date/time   comparisons, year(d) month(d) day(d), date(text), timestamp(text)
generative  uuid() now()        (write-time positions only; recorded in $gen)
aggregates  count sum avg min max distinct    (over view fields)
control     assert(cond, message)
keys        hash(x) canonical content hash; row keys via binding.$key
absent      none, has(x), ??
```

Existence tests compose from what exists: `count(...) > 0`, no `exists`
macro. Anything not listed is not in profile v1.

## 2. Aggregation typing — resolved

In `SYNTAX.md` §10: `group` binds the same-key source rows; aggregates are
typed by element (`int` stays `int`, `decimal` stays `decimal`); absent
values are skipped; empty input gives `count 0`, `sum` zero of the element
type, and absent `avg/min/max` (the output field is optional); mixing numeric
kinds is impossible by typing; diagnostics carry the source row chain identity.

## 3. Composite-key encoding — resolved

In `COLLECTIONS.md` §5: canonical scalar text per key field in `$key` order,
`%` and `:` percent-escaped inside text parts, parts joined by `:`.
Deterministic, reversible given the key type, stable across engines, used
identically in `$data` member names and path display.

## 4. Sort details — resolved

```text
text order    codepoint (binary) order; deterministic on Postgres
              (COLLATE "C") and in memory
case folding  no collation options in v1: sort by a computed field
              ("name_ci": "= lower(.name)", "$sort": ["name_ci"]) —
              composable, zero new syntax
absent        sorts as greater than every present value
              (NULLS LAST ascending, NULLS FIRST descending)
stability     row identity is always appended as the final tiebreak,
              making every sort a total order everywhere
```

## 5. Reference delete behavior — resolved

There is **no default**. Deletion policy is declared on the ref:

```text
{ "$ref": "#people", "$on_delete": "restrict" }
{ "$ref": "#people", "$optional": true,
  "$on_delete": "= { person: none }" }
{ "$set": { "$ref": "#people", "$on_delete": "cascade" } }
```

A ref without `$on_delete` is legal only while no mutation group, migration,
or exposed surface can delete its target. The checker runs before exposing a
mutation, so cross-module surfaces get the same rule for free: the module that
exports a deleting mutation must decide all affected inbound refs, or the
surface is not loadable. History always preserves old observations until
erasure removes bytes vertically.

## 6. Direct import resolution — resolved

In `MODULES.md` §6: candidates are instances exposing a compatible public
surface, visible by walking up ancestor module spaces from the install
location; one candidate binds automatically; several require an explicit
binding; zero blocks a required import and leaves an optional one absent; the
binding persists in the install record and is rebindable.

## 7. Optional import runtime shape — resolved

In `MODULES.md` §6: absence propagates. `has(#billing)` tests presence; any
expression reading an absent import is absent; derived fields, views, and
exposes that read it are absent. Stored private data is untouched, so nothing
is archived or restored — optional imports need no lifecycle machinery.

## 8. Install record — resolved

Locked in `MODULES.md` §11: `$module`, `$source` (sha256 of the canonical
package document), `$resolved` (handle → `$parent.<surface>` or
`<instance path>#<surface>`), `$absent`, `$migrations` (from/to/commit).

## 9. Module update reports — resolved

Locked in `MODULES.md` §13: instance path, from/to versions, migrated paths
with row counts, seed merge results, exposed-interface diff, import
rebinds/breaks, archived paths, and the commit id. A breaking interface
recheck blocks the update before any commit is admitted.

## 10. Bundles — resolved

In `HISTORY.md` §8. A module *package* is `$model` + `$data` (+ `$use` /
`$expose`) and is what registries distribute; a *bundle* is an export of live
state — `$bundle` header with `$heads` frontier, snapshot/log/full modes,
module instances included with their install records, and optional
`$packages` embedding the source packages by hash so imports never depend on
a registry.

## 11. Postgres lowering — deferred by design

The authoring surface is logical and this stays an engine document. The
contract only fixes what authors can observe:

```text
one physical table per keyed-collection path, hidden parent identity
refs lower to indexed key columns; $unique to unique indexes
views materialize or compute per engine choice; watch semantics identical
  either way (the feed is the fold), so engines may upgrade from
  recompute-on-read to incremental maintenance invisibly
collection replacement and bulk patches are single transactions
sort lowers to ORDER BY with COLLATE "C" and explicit NULLS placement
```

## Consistency fixes applied alongside (v0.2 → v0.4)

```text
expression positions      bare vs =-marked, stated once (SYNTAX §1)
literal escape            leading ' strips one character and forces a
                          literal in literal-or-expression positions
Hjson authoring           allowed as source input; canonical package JSON
                          remains strict and hashable
primitive/type tables     canonical primitive table plus full type syntax
                          table added to SYNTAX §1a
projection scoping        . is always the source row; output fields become
                          bindings; §10 example corrected
computed scalar fields    restored: "field": "= expr" (COLLECTIONS §3b)
$mut placement            declarable on collections and structs, not only
                          views — parent-surface $mut lists resolve by name
mutation sequences        array bodies, one atomic outcome (MUTATIONS §11)
concurrency               read basis derived from evaluation; conflicts are
                          rejected before admission, never empty commits
purity                    fold-time pure; uuid()/now() write-time only,
                          recorded in $gen (CHECKS §6)
history access            model-visible $history retained; reader-time
                          history accessors removed pending a fuller UX model
migrations                collection $from, optional $back inverse,
                          downgrade = restore + archive (CHECKS §5)
history/bundles           ported and integrated (HISTORY.md)
```
