# §20 Package evolution and migrations — chapter notes

This directory covers SPEC.md §20 (anchor [`#evolution`](../../SPEC.md#evolution)):
schema migration (§20.1), reversible transforms and downgrade (§20.2), and
compatibility / update checking (§20.3), together with the compatibility
algorithm §20.3 delegates to Annex E and the load-outcome rules of §9.

## Reused extension step: `host_load`

§20 has no lifecycle vocabulary of its own; a package update is the §9.2 host
operation `load(target, artifact)` ("update an existing package instance from
the artifact definition"). This chapter therefore **reuses the `host_load`
step defined and documented in
[`09-loading-bootstrap/NOTES.md`](../09-loading-bootstrap/NOTES.md)**:

```hjson
{ host_load: { package: { ...new definition... } },
  expect: { outcome: ok, result: committed } }
```

- The harness builds a `.liasse` artifact from the inline `package` and runs
  `load` against the case's active root instance (§9.2).
- On `expect.outcome: ok` the extra member `result` is the §9.4 lifecycle
  outcome (`committed` or `unchanged`).
- A §9.4 `rejected` lifecycle result is expressed with the FORMAT.md non-ok
  vocabulary plus `violates`; after it the prior active composition is still
  in force (§9.2, §9.4, Annex E.9) and later steps assert against it.

### Outcome mapping (identical to ch. 9)

§9.4 collapses every failed update to the single lifecycle result `rejected`.
FORMAT.md's finer vocabulary is applied exactly as ch. 9 does:

- `invalid` — the new definition is statically bad on its own terms: syntax,
  typing, unknown members, an impure/non-deterministic migration transform,
  a write to read-only `$old`, or a compatibility narrowing detectable from
  the two definitions alone (§9.2 steps 1–6, Annex E.3).
- `rejected` — the definition is well-formed but the **prospective migrated
  state** violates an admission-class constraint: keys, refs, uniqueness,
  checks, a failed reversible round trip, or an unrepresentable downgrade
  value (§9.2 steps 7–8, §20.1 final-check clause, §20.2).

Both map to §9.4 `rejected`; the split is only FORMAT.md's vocabulary. Where a
case's classification could be argued, its `note` says so — the non-ok outcome
itself is never in doubt. Neighboring chapter 13 records §13.14 narrowing as
`rejected`; this chapter classifies a definition-only compatibility narrowing
as `invalid` because it is decidable from the two definitions with no state,
per the ch. 9 mapping. The observable lifecycle result is `rejected` either
way.

## Reused steps from chapter 19

`downgrade-preserves-history-order` reuses `export` and `inspect_artifact`
exactly as documented in [`19-history-artifacts/NOTES.md`](../19-history-artifacts/NOTES.md)
to read the §19.6 `history_index` and observe that a package downgrade does
not rewrite lineage identity or origin (§20.2 "history order remains
unchanged").

## Authoring conventions specific to this chapter

- **Target packages carry no `$data`.** A migration case seeds state only on
  the genesis (older) package. The update target defines no `$data`, so the
  post-update state is produced purely by the migration machinery (compatible
  same-identity copy, `$from` mappings, `$migrations` program) and nothing
  else. How a *new* package's `$data` seed would interact with a root-app
  migration is itself unspecified and is captured in
  `common/target-seed-data-on-update-unspecified`.
- **`int` wire form.** Per Annex A.1 an `int` value travels as a base-10 JSON
  *string*; an expected length of one is written `"1"`.
- **`base64` fixture.** `base64.encode(string.bytes("hi"))` == `"aGk="`
  (bytes `0x68 0x69`); used by the reversible-transform cases.
- **Unicode.** `red/confusable-from-source-name-nonexistent-rejected` embeds a
  literal non-ASCII confusable in a `$from` value; the codepoints are spelled
  out in that case's `note` so a mangled file is detectable.

## Spec gaps recorded as `outcome: unspecified`

1. `common/target-seed-data-on-update-unspecified` — §20 defines migration
   over `$old`/`.` but never states how a target package's own `$data` seed
   interacts with a root-application update. §13.13's three-way seed merge is
   defined for *module* updates only; nothing extends it to the root app.
2. `red/sequence-composition-not-mandated-unspecified` — §20.1 says a runtime
   **MAY** compose a sequence of package versions when every adjacent target
   supplies a migration from the preceding active version. "MAY" leaves it
   optional: whether a lone runtime accepts an update that would require such
   composition (rather than a direct migration keyed to the exact active
   source version) is not mandated either way.
