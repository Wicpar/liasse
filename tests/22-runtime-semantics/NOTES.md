# §22 Runtime semantics — chapter notes

Corpus extensions used by this chapter's cases, per FORMAT.md ("A chapter may
need an action this vocabulary lacks. Use a new, descriptive step key, and
document its semantics in `tests/<chapter>/NOTES.md`."). Most conventions are
reused from the §12 chapter (`tests/12-clients-live-views/NOTES.md`); they are
restated here only where this chapter depends on them.

## Reused step members

- **`expect.completion`** — `committed | unchanged`; asserts which §12.3 / §8.9
  completion a successful (`outcome: ok`) response reported. Used here to pin
  the §22.2 rule "a program producing no state change returns `unchanged` and
  creates no commit": the completion is `unchanged` and no live view advances.
- **`concurrently: [ [steps...], [steps...] ]`** and **`expect_one_of`** — an
  interleaving-unspecified admission race (FORMAT.md). Used for §22.3/§22.4
  ordering cases where the spec admits several serializations. `expect_one_of`
  is used both per-branch inside `concurrently` (as FORMAT.md shows) and as the
  assertion body of a following `expect_view` step, where it enumerates the
  finite set of serial-order-dependent final views the spec permits (e.g.
  `{ ab, ba }` for two commutative-but-order-visible writes). Every enumerated
  alternative is a spec-conformant outcome; the case passes when the observed
  view matches exactly one of them.
- **`restart: {}`** — stop and replay the runtime; durable state must survive
  (FORMAT.md). Used for the §22.1 "Replay uses those recorded values" and
  §22.7(1) "durably commit" rules.
- **`advance_time: "<ISO-8601 duration>"`** — moves the virtual clock, which
  starts at `2026-01-01T00:00:00Z` (FORMAT.md determinism). Package default
  timestamp precision is `us` (Annex A.5 "The package default is `us`") unless
  `$semantics.timestamp_precision` overrides it, so `now()` at genesis is
  `1767225600000000` (µs since epoch) and wire timestamps are canonical base-10
  integer strings (Annex A.1/A.5), matching the established convention in
  `tests/06-expressions/common/`. `int`-typed values (e.g. `count(...)` results,
  `balance`) are likewise canonical base-10 JSON strings per Annex A.1, not JSON
  numbers.
- **`host_load: { package: { ... } }`** with **`expect: { outcome: ok, result:
  committed | unchanged }`** — the §9.2 host lifecycle operation
  `load(target, artifact)`. This chapter **reuses the `host_load` step defined
  and documented in [`09-loading-bootstrap/NOTES.md`](../09-loading-bootstrap/NOTES.md)**
  (as `tests/20-evolution-migrations/` also does). Used here for the §9.3/§2.4
  rule that a definition-only update creates a commit even when the writable
  state delta is empty.

## `hosts` conventions (authentication cases)

Actor-provenance cases (§22.8) exercise the real §11 authenticator pipeline.
They follow the `tests/11-auth-sessions/` convention:

- **`$requires: { token: "test.token@1" }`** on the package plus
  **`hosts: { token: { $namespace: "test.token@1", tokens: { ... } } }`** — a
  simulated host token namespace. `token.verify($credential)` resolves a
  credential string to a `{ auth, session }` record; the package's `$auth`
  declaration maps that to a session row and an `$actor` account row.

## Observed spec gaps

Captured as `outcome: unspecified` cases (see each file's `note`):

- `red/now-half-precision-rounding-mode-unspecified.hjson` — §22.5 / Annex A.5
  delegate precision conversion to "the PostgreSQL timestamp precision rule" but
  do not reproduce its half-way tie-break in SPEC.md's own text. Unlike decimals
  (Annex A.6 explicitly resolves a halfway value "away from zero"), the
  timestamp tie-break at an exact half-unit boundary is not stated, so the
  recorded whole-second value is not deducible from SPEC.md text alone.
- `red/public-request-references-actor-unspecified.hjson` — §22.8 / §10.2 say
  public requests "run without an actor", but the spec does not pin what happens
  when a public mutation *references* `$actor`: §6.2 (structural bindings "exist
  only in the feature context that defines them") supports static rejection at
  load; §6.3 (a `$actor` context "MUST receive exactly one occurrence. Zero …
  reject that evaluation") supports runtime rejection at admission; a null/absent
  binding is a third defensible reading. SPEC.md chooses none.
- `red/cross-connection-sequential-order-unspecified.hjson` — §22.3 lists
  causal completion only as one *optional* precedence event ("Ordering events
  MAY include causal completion…"). When request B on one connection is issued
  after request A on a different connection has already returned, the spec does
  not require the admission mechanism to treat that wall-clock causality as
  precedence, so the committed serial order of A and B is not pinned.

