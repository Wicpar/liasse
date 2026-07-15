# Annex D — Canonical identity, paths, integrity — chapter notes

This file documents the step vocabulary these cases use beyond
`tests/FORMAT.md`. Grounding: SPEC.md Annex D (§D.1–§D.8), plus the feature
chapters Annex D refers to: §9.1 (seed data / canonical encoded key text),
§12 (operation identity), §19.5–§19.9 (portable artifacts, verification,
reconciliation), and §21.2–§21.3 (erasure / reinsertion).

## Steps reused from the §19 chapter

The following steps have identical semantics to the ones documented in
`tests/19-history-artifacts/NOTES.md`; the runtime and harness treat them the
same everywhere. They are listed here only so this chapter is self-describing:

- `export` / `import` / `reconcile` — artifact production and movement
  (§19.7, §19.8, §19.9). Result objects are always matched with `"...": true`
  because the concrete Rust API shape is implementation-defined (§19.9).
- `restore` — instantiate an application from an artifact in a runtime that
  has no installed instance (§19.10). Restoration performs the same complete
  recursive artifact verification as import (§19.8, §D.5). Verification
  failures surface on this step as `outcome: invalid`.
- `in_sandbox` / `in_sandbox … fresh: true` — run nested steps against a
  separate, initially empty runtime ("another host"). Artifact labels and
  `$bind` bindings are case-global; connections and state are per-runtime.
- `inspect_artifact` — assert on decoded artifact content without importing
  it. `manifest` is the decoded `manifest.json` (§19.5), so
  `manifest.definition.identity` is the canonical definition identifier of
  §D.4. Members of one `expect` object are evaluated in listed order, so an
  earlier member may `$bind` a name a later member `$ref`s.
- `tamper_artifact` — derive a new labeled artifact by applying deterministic
  edits to a copy (`edit_cbor`, `edit_json`, `set_entry`, `fix_checksums`,
  etc.); the source label is untouched. Op semantics are exactly those in
  `tests/19-history-artifacts/NOTES.md`.
- `apply_correction` — run the host correction function of §19.9 against a
  bound reconciliation plan. `choose` maps a **display path** (§D.3) to
  `"local"`, `"incoming"`, or `{ value: <typed-value> }`. This chapter uses it
  to assert the §D.3 display-path encoding, since a display path is only
  externally addressable through a correction.

## Steps added by this chapter

### `op_id` member on `call`

```hjson
{ call: "public.tasks.add", args: { title: "x" }, op_id: "op-7",
  expect: { outcome: ok, "...": true } }
```

`op_id` attaches the external high-entropy operation identifier of §12 / §D.8
to the call. Two `call` steps that carry the **same** `op_id`, target the
**same** public-or-scoped-role surface, use the same selected authenticator,
and send an **equivalent** request model one operation submitted twice (a
transport re-delivery / retry); §12 and §D.8 require at-most-once execution
for that pair. A `call` with no `op_id` is a new operation on every
submission (§12: "A call without an identifier is a new operation on every
submission"). Because mutation return values are ephemeral (§D.8), retry
steps assert the observable committed state, not a pinned return value; the
retry's own result object is matched with `"...": true`.

### `erase` and `reinsert`

```hjson
{ erase: { call: "public.notes.scrub", args: { id: "n1" }, bind_extract: "e1" },
  expect: { outcome: ok } }
{ reinsert: { extract: "e1" }, expect: { outcome: ok } }
```

`erase` invokes an application-declared, explicitly exposed erasure call
(§21.2: "Authorization uses an explicitly exposed erasure call") — a public
mutation whose program contains `erase(row)`. `bind_extract` captures the
durable erasure extract returned by the operation (§21.2 step 6, §D.7) under
a case-global label. `reinsert` runs `reinsert(extract)` (§21.3) against a
previously bound extract; it verifies the extract content hash, attestations,
referenced erasure history point, and each occurrence's current digest stub,
and restores bytes only where the exact expected stub remains (§21.3, §D.7).

## Conventions

- All non-`ok` step outcomes carry `violates`, including `unspecified`, where
  `violates` names the interacting rules whose combination leaves the
  behavior unpinned and `note` explains the gap.
- Artifact / extract verification failures use `outcome: invalid` (statically
  rejected at verification time), consistent with the §19 chapter.
- Typed key values sent in `args` and returned in view rows use their typed
  JSON wire form, never the §D.2 encoded key text. The encoded text appears
  only as `$data` seed member names, display-path segments, and canonical
  textual exports (§D.2: "Expressions use typed key values rather than
  encoded strings").
