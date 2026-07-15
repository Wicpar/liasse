# §19 History, artifacts, reconciliation — chapter notes

This file documents the step vocabulary this chapter adds beyond
`tests/FORMAT.md`, plus the argument/result conventions the cases use for the
`export` / `import` / `reconcile` steps whose payloads FORMAT.md leaves open.
Grounding: SPEC.md §19 (history, artifacts, reconciliation) and Annex D
(canonical identity, paths, integrity).

## Conventions for existing steps

### `export`

```hjson
{ export: { as: "a1" } }
{ export: { as: "a1", lineages: "all" } }
{ export: { as: "a1", instance: "mods/m1" } }
```

- `as` labels the produced artifact for later steps. Labels are case-global.
- Default scope: the root instance, its active composition, and the complete
  retained history (§19.7 lets the host select; `$history` defaults to `all`,
  §19.3). `lineages: "all"` makes the inclusion of every retained lineage
  explicit where a case asserts on alternate lineages.
- `instance` exports one child instance by `<module-space>/<instance-name>`
  (§19.7 "Exporting one child").

### `import` and `reconcile`

```hjson
{ import: { from: "a1", policy: ["fast_forward", "rollback", "merge"] },
  expect: { outcome: ok, value: { relation: "fast_forward", applied: true, "...": true } } }

{ reconcile: { from: "a2", policy: ["merge"], bind_plan: "r1" },
  expect: { outcome: ok, value: { relation: "merge", applied: false, conflicts: "$any", "...": true } } }
```

- `policy` lists the automatic movements allowed to activate (§19.8: an
  import policy selects fast-forward, rollback, merge, branch creation,
  unrelated replacement).
- Result value convention (the Rust API shape is implementation-defined per
  §19.9, so the harness adapts the native result to this logical shape):
  - `relation` is the §19.8 classification of the incoming artifact against
    local retained history: `same_point` | `fast_forward` | `rollback` |
    `merge` | `unrelated`.
  - `applied` is whether a movement activated. A relation whose movement is
    not in `policy` classifies but does not activate (`applied: false`).
  - `conflicts` is present and non-empty when a merge failed and returned a
    reconciliation plan (§19.9). Its element shape is implementation-defined;
    cases match it with `"$any"` only.
- `reconcile` is `import` that attempts the three-way merge path of §19.9 and
  keeps the reconciliation plan when the merge fails. `bind_plan` binds that
  plan for a later `apply_correction`.
- Cases always match result objects with `"...": true`, because the concrete
  API shape is implementation-defined.

## Extension steps (documented per FORMAT.md)

### `restore`

```hjson
{ restore: { from: "a1" }, expect: { outcome: ok } }
```

Instantiates an application from an artifact in a runtime that has no
installed instance (§19.10 "Restoring an artifact"). Restoration performs the
same complete recursive artifact verification as import (§19.8, Annex D.5),
so tampered-artifact cases assert their failure on this step. The restored
instance keeps the artifact's instance incarnation, selected point, and
retained history.

### `in_sandbox`

```hjson
{ in_sandbox: "s1", steps: [ ... ] }
{ in_sandbox: "s2", fresh: true, steps: [ ... ] }
```

Runs the nested steps against a separate, initially empty runtime ("another
host"). Artifact labels and `$bind` bindings are case-global and shared with
the sandbox; connections and state are per-runtime, and the implicit
single-client connection rule applies inside each runtime independently.
`fresh: true` first instantiates the case's (root) package as a brand-new
application in the sandbox — a new genesis with a new instance incarnation —
instead of expecting a `restore`. Repeated `in_sandbox` with the same id
continues the same sandbox runtime.

### `inspect_artifact`

```hjson
{ inspect_artifact: { artifact: "a1", expect: {
    outcome: ok
    mimetype: "application/vnd.liasse+zip"
    manifest: { ... }              // decoded manifest.json (§19.5)
    history_index: { ... }         // decoded history/index.json (§19.6)
    selected_lineage: { ... }      // history_index.lineages[history_index.selected.lineage]
    lineage_heads: { $unordered: [ ... ] } // set of `head` across all retained lineages
    entry_names: { $unordered: [ ... ] }   // archive entry paths
} } }
```

Asserts on decoded artifact content without importing it. Every asserted
field is defined by the normative artifact formats (§19.5, §19.6, Annex D.5).
Matchers and `$bind`/`$ref` apply; members of one `expect` object are
evaluated in listed order so earlier members may bind names used by later
ones. Omitted fields are unchecked.

### `extract_artifact`

```hjson
{ extract_artifact: { from: "a1", entry: "modules/*.liasse", as: "a2" } }
```

Extracts one archive entry that is itself a `.liasse` artifact (§19.5: every
entry below `modules/` is a complete artifact) and labels it. `entry` may be
a glob; it must match exactly one entry or the step is a corpus error.

### `tamper_artifact`

Canonical registry step owned by this chapter (see the **Extended step
registry** in `tests/FORMAT.md`); the full op vocabulary is defined here.

```hjson
{ tamper_artifact: { from: "a1", as: "a1x", ops: [ ... ] } }
```

Derives a new labeled artifact by applying deterministic edits to a copy; the
source label is left untouched. Ops, applied in order:

- `{ corrupt_entry: { path } }` — flip the last byte of the entry's bytes.
- `{ set_entry: { path, text } }` — replace the entry bytes with UTF-8 text.
- `{ remove_entry: { path } }` — delete the archive entry (manifest untouched).
- `{ duplicate_entry: { path } }` — add a second archive entry with the same
  name and identical bytes.
- `{ add_entry: { path, text } }` — add a new entry with UTF-8 text content.
- `{ copy_entry_from: { artifact, path } }` — replace the entry with the
  bytes of the same-named entry from another labeled artifact.
- `{ edit_json: { path, pointer, value } }` — decode a canonical-JSON entry,
  set the member at the JSON pointer (creating it if absent), re-encode
  canonically.
- `{ duplicate_json_member: { path, pointer, new_name } }` — duplicate the
  object member selected by `pointer` under `new_name` in the same object.
- `{ edit_cbor: { path, pointer, value } }` — decode a `.cbor.zst` entry,
  set the member at the logical pointer, re-encode as canonical CBOR +
  Zstandard. Pointer segments through keyed collections use canonical key
  text (Annex D.2).
- `{ rewrite_identifier: { from, to } }` — replace an identifier string
  everywhere it appears as a value (or member name) in the decoded JSON and
  CBOR structures of the artifact, recursively including nested module
  artifacts.
- `{ fix_checksums: true }` — recompute every `manifest.json` checksum
  (`entries`, `state`, `history`, `included_modules`, and nested manifests)
  so they match the tampered bytes. Used to isolate a semantic rule from
  plain checksum failure.
- `{ add_manifest_entry: { path } }` — add an `entries` member for `path`
  with the correct media type and sha256 of the current bytes.

In `path`/`pointer`, `*` selects the canonically first member when several
match; a glob that matches nothing is a corpus error.

### `module_install`

Registry step (canonical name `module_install`, owned by §13); see the
**Extended step registry** in `tests/FORMAT.md`. This chapter's child-module
cases install into a module space with
`{ module_install: { space: "mods", request: { $name, $module } } }`.

### `apply_correction`

```hjson
{ apply_correction: { plan: "r1", choose: { "/notes/n1/body": "incoming" } },
  expect: { outcome: ok, value: { applied: true, "...": true } } }
```

Runs the host correction function of §19.9 against a bound reconciliation
plan. `choose` maps a display path (Annex D.3) to `"local"`, `"incoming"`, or
`{ value: <typed-value> }`. The corrected composition is validated and
activated atomically per §19.9.

### `expect_one_of` inside `expect_init`

Registry member (owned by §12/§23); see the **Extended step registry** in
`tests/FORMAT.md`. A `watch`'s `expect_init` may carry an `expect_one_of`
listing the spec-allowed initial results after a `concurrently` race:
`{ watch: "public.notes", id: "w1", expect_init: { expect_one_of: [ [ ... ], [ ... ] ] } }`.

## Other conventions

- All non-`ok` step outcomes carry `violates`, except `unspecified`, which
  carries no `violates` (per `tests/FORMAT.md`) and instead names the
  interacting rules whose combination leaves the behavior unpinned in a
  `note`/`detail`.
- Artifact-verification failures on `restore`/`import` use
  `outcome: invalid` (statically rejected at validation time, before any
  movement is classified or applied).
