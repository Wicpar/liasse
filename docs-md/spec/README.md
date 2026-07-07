# Liasse docs — v0.4

Status: standard draft. v0.4 locks the dependency model (`$deps`, peers at usage sites, `$if`, blocked updates with explicit remedies, side-by-side majors), view bounds and engine-driven windows, the trusted/untrusted client split, delta-tree commits with virtual `$prior`, the `$id`/`$state` integrity split, explicit `$on_delete` with static completeness, erasure as revertible extraction, `$history` as the model-visible history surface, the harmonized shape model (types = shapes = interfaces), blobs and storage, limits and sources, permissions/users/sessions, view combinators, named types and enums, and the derived-basis atomicity rule.

Canonical package artifacts are strict JSON. Authoring tools may accept Hjson as a source format, but the engine first parses it to the same strict JSON package tree before validation, hashing, or loading. Hjson is an authoring convenience only: comments, optional commas, unquoted keys, quoteless strings, and triple-quoted multiline strings do not add language semantics.

Documentation convention: `json` fences are complete canonical JSON examples; `hjson` fences are authoring snippets or fragments that are normalized before loading.

This set captures the redesigned authoring surface for a Rust-first, Postgres-backed, strongly checked dynamic config/data ORM.

## Files

- `SYNTAX.md` — access roots, expression positions, type syntax, row binding, view projection, refs, mutation operators, synthetic keyed views, and sort syntax.
- `COLLECTIONS.md` — static structs, keyed collections, sets, unique relations, generated keys, views as collection shapes, and `$sort`.
- `MODULES.md` — module packages, row-scoped module spaces, parent-provided surfaces, direct imports, exposed interfaces, and views into installed modules.
- `MUTATIONS.md` — mutation examples using the target-first operator syntax, derived basis, and delete completeness.
- `CHECKS-TRANSFORMS.md` — check syntax, current-value semantics, normalization, migration transforms, and purity.
- `LIMITS.md` — meters: `$consumes`, `$limits`, `$sources`, drain order, hierarchy, zero default, and cross-module contracts.
- `PERMISSIONS.md` — identities, accounts, `$roles`, `$members`, surfaces, definer authority, sessions, and the modeled login system.
- `STORAGE.md` — the `blob` primitive, stores as rows, `$blob_storage`, reconciliation, blob meta accessors, integrity, retention, and real-location billing.
- `CLIENT.md` — the engine-served client: surface manifests, names-only requests, untrusted pipeline, live views, and transactional calls with streaming blobs.
- `HISTORY.md` — commits, converging DAG, derived read basis, rejection/no-admission, watch, bundles, and erasure.
- `examples/accounting-templates.md` — concrete company-local module-space example with accounting template data packs.
- `RESOLUTIONS.md` — resolved design questions and the ledger of consistency fixes.

## Current authoring surface

```text
package/app/module
  $liasse, $app, $module, $model, $data, $types

types and shapes
  primitive names, named shape use, $type, $optional, $default,
  $check, $normalize, $enum, $ref, $on_delete, $set, $key,
  $sort, $unique, $like, $view

mutations and permissions
  $mut, signature params, $roles, $members, surface entries
  ($view plus optional $params and $mut list)

modules
  $modules, $modules.$expose, $modules.$interfaces, $use,
  $use.$optional, $deps, $expose, $if

limits and storage
  $consumes, $limits, $sources, $eligible, $order,
  $blob_storage, $in, $serve, blob parameter constraints

history and migration
  $history middleware, $from, $as, $back, extraction/reinsert bundles
```

## Core syntax

```text
/                         package root
.                         current value/object
^, ^^, ^^^                lexical parent traversal
#name                     imported surface
@name                     parameter
name                      local binding
none                      absent/no value in expressions

collection[key]           one row by typed key expression
collection[a, b, c]       ordered multi-selection by typed key/keyset/view expressions
collection[:x]            bind rows as x
collection::              same-name row binding

View construction:
  { field, field: expr, @field, source.field, binding: }

Mutation patch:
  { field = expr, @field, source.field, .field, -field }

Collection insert:
  collection + view_expression

Collection replacement:
  collection = view_expression

Collection patch:
  row_source { patch_block }

Collection delete by key:
  collection - keys

Selected-row delete:
  -row_source

Computed field:
  "field": "= expr"

Projection scoping:
  . is the source row; output fields become bindings; group in keyed views

Mutation sequences:
  $mut bodies may be arrays: one atomic outcome

Concurrency:
  read basis derived from evaluation; contention rejects the proposal,
  never creates an empty commit
```

## Design intent

```text
Modules are rooted where they are installed.
A module space inside a row is local to that row.
Shared modules live at a shared location.
Views are data.
Refs are checked key types pointing at data.
Backend flattening is invisible to authors.
```
