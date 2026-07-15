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
  ordering cases where the spec admits several serializations.
- **`restart: {}`** — stop and replay the runtime; durable state must survive
  (FORMAT.md). Used for the §22.1 "Replay uses those recorded values" and
  §22.7(1) "durably commit" rules.
- **`advance_time: "<ISO-8601 duration>"`** — moves the virtual clock, which
  starts at `2026-01-01T00:00:00Z` (FORMAT.md determinism). Package default
  timestamp precision is `us` unless `$semantics.timestamp_precision` overrides
  it, so `now()` at genesis is `1767225600000000` (µs since epoch) and wire
  timestamps are canonical base-10 integer strings (Annex A.1/A.5), matching
  the established convention in `tests/06-expressions/common/`.

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

- `red/now-half-precision-rounding-mode-unspecified.hjson` — §22.5 says
  precision conversion "rounds to the requested fractional-second precision"
  but never fixes the rounding mode (half-up, half-even, truncate). A `now()`
  landing exactly on a half-unit boundary therefore has an unspecified
  recorded value.
- `red/public-request-references-actor-unspecified.hjson` — §22.8 says public
  requests "run without an actor", but the spec does not pin what happens when
  a public mutation *references* `$actor`: static rejection at load, a runtime
  error at admission, or a null binding are all defensible readings.
- `red/cross-connection-sequential-order-unspecified.hjson` — §22.3 lists
  causal completion only as one *optional* precedence event ("Ordering events
  MAY include causal completion…"). When request B on one connection is issued
  after request A on a different connection has already returned, the spec does
  not require the admission mechanism to treat that wall-clock causality as
  precedence, so the committed serial order of A and B is not pinned.
</content>
</invoke>
