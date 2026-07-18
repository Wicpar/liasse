# §8 Mutations and validation — corpus notes

Chapter anchor: [`#mutations`](../../SPEC.md#mutations) (SPEC.md §8), with
supporting citations into §5 (state model), §6 (expressions), §10 (surfaces),
§21 (deletion), §22 (runtime semantics), and Annexes A/B.

## Extensions beyond FORMAT.md

### `completion` member in a step `expect` (expectation extension, not a step key)

§8.9 and §12.3 distinguish two *success* completions of a call:

- `committed` — a commit was created and is final;
- `unchanged` — the program produced no state change, no commit was created,
  and the client frontier does not advance; a trailing `return` is still
  evaluated against the unchanged state.

FORMAT.md's outcome vocabulary folds both into `ok`. Cases in this chapter
that must discriminate them add the canonical `completion` member (registry
member owned by §12; see the **Extended step registry** in `tests/FORMAT.md`)
next to `outcome`:

```hjson
expect: { outcome: ok, completion: unchanged, value: [] }
```

`completion` is only ever `committed` or `unchanged`, only appears together
with `outcome: ok`, and is omitted when the distinction is irrelevant to the
rule under test. Used by:

- `common/zero-match-bulk-patch-returns-unchanged`
- `common/set-noop-add-remove-returns-unchanged`
- `red/idempotent-write-returns-unchanged-no-commit`
- `red/omitted-optional-param-clears-field`

No new *step keys* were invented; all steps use the FORMAT.md vocabulary
(`connect`, `call`, `watch`, `expect_view`, `restart`, `concurrently`,
`expect_one_of`).

## Authoring conventions in this chapter

- **Wire values.** Expected values and call arguments use the canonical
  strict-JSON wire forms of Annex A.1: `int`, `decimal`, and `timestamp`
  values are canonical base-10 JSON strings (e.g. `balance: "100"`), `bool`
  is a JSON boolean, `uuid` is a lowercase hyphenated string. `$data` scalar
  values likewise use canonical values per §9.1.
- **Seeds.** Required root-scalar fields are seeded explicitly through
  `$data` even when a default exists, so no case depends on whether genesis
  resolves root defaults — that question belongs to §9's corpus.
- **`red/unicode-confusable-keys-are-distinct`** writes its key strings with
  literal `\u` escape sequences (the file bytes are ASCII there) so the NFC
  and NFD scalar sequences cannot be conflated by editors or tooling.

## Spec ambiguities found (outcome: unspecified)

1. `red/delete-absent-key-outcome-unspecified` — §8.9 rejects a keyed row
   *patch* whose target is missing and lets zero-match *filtered* bulk
   operations succeed as `unchanged`, but never assigns an outcome to
   delete-by-key (`collection - key`) when the key is absent. §6.3 selector
   semantics (absent key contributes zero rows) suggest `unchanged`; the
   keyed-patch precedent suggests rejection.

**Resolved (SPEC-ISSUES #6).** `red/unknown-argument-name` is no longer
unspecified: §12.1 now pins the external argument object as a closed shape, so a
member that is not a declared parameter makes the request malformed and is
rejected at parameter parsing, before admission. The case asserts `rejected`.

## Known coverage gaps (deliberate)

- §8.2 "A mutation declared on a view uses the view's lexical declaring
  scope and MAY target the underlying model explicitly" — needs §7 view
  scoping machinery and the spec gives no worked observable shape to pin;
  deferred to a cross-chapter case once view-declared mutations get one.
- §8.11 "Internal calls ... preserve the external request's `$actor` and
  `$session` bindings" — observably testable only with §11 authentication
  fixtures; belongs to the authentication/roles corpus. The same-atomic-
  program half of §8.11 is covered by
  `red/internal-call-failure-rejects-caller-writes`.
- §8.12 provider-backed generated operations ("accepted only in write-time
  mutation positions") and namespace audit projections — require §16 host
  namespace fixtures (`hosts:`); the pure-position effect-class rule is
  covered by `common/pure-position-effect-class-invalid` instead.
- §8.4 "An insertion from a multi-row view returns the inserted row view in
  source order" — not isolated; adjacent ordering rules are covered by
  `red/delete-selector-duplicates-first-occurrence` (selector-order returns)
  and `red/replacement-validates-complete-collection` (replacement result).
- §8.7 "Where a feature must allocate among several affected rows ... it
  uses the source view's declared row order" — the only observable allocator
  is meters (§15); belongs to the §15 corpus.
- §8.6 nested patch blocks descending into struct fields — subsumed by the
  flat patch cases; no distinct normative behavior beyond field addressing.
