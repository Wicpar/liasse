# §15 Meters — corpus notes

## Extension steps

This area introduces **no** step keys beyond the FORMAT.md vocabulary. Every case
uses only documented steps and step members:

- `connect`, `call`, `watch` (with `expect_init`), `expect_view`, `advance_time`,
  `concurrently` (with per-branch `expect_one_of`).

No `tests/15-meters/`-local step key is defined, so the harness has no undocumented
key to reject. If a future meter case needs an action the vocabulary lacks, add the
descriptive key here with its semantics.

## Matchers of note

- `funding` allocations are asserted with `{ $unordered: [...] }` and per-row
  `"...": true`, because SPEC §15.3 fixes the *set* of funding rows and their
  `source`/`amount`, while pool identity and interval bounds are additional recorded
  members the case does not need to pin.
- Several cases verify allocation *shape-independently*: rather than asserting an
  internal split, they advance past a pool's expiry and read the surviving
  `.credits.balance`, which is only consistent with one correct split (see
  `common/unbucketed-source-defaults-to-unbounded-interval`,
  `common/w3-overlapping-heterogeneous-credits`).

## Anchor conventions

- A spend that finds **no eligible capacity because every candidate pool is temporally
  inactive** (`expired-pool-does-not-fund-current-spend`, `spend-time-in-gap-between-periods-unfunded`,
  `spend-at-pool-until-boundary-excluded`, and the current-time step of
  `backdated-spend-consumes-expired-pool`) cites BOTH the temporal rule that empties the
  meter (§15.1 spend-time source evaluation, or §14.1 half-open activity) AND §15.2 step 6,
  the operative rule that "rejects the complete transition when eligible capacity is
  insufficient". The temporal rule is the cause; §15.2 is the rejection.

## MUST rules intentionally left uncovered

- SPEC §15.2 "A source view that repeats the same full pool identity contributes one pool …
  Repeated occurrences MUST agree on quantity, interval, and projected funding metadata;
  disagreement rejects the admission." This duplicate-pool coalescing rule is not exercised:
  producing the *same full pool identity twice from one source view* requires a projection
  that emits duplicate identities, which ordinary keyed-collection and source-backed-bucket
  projections (each row a distinct key/`$source` identity, §14.6) do not do. A minimal,
  spec-clear construction that forces a genuine duplicate could not be built without relying
  on unstated expression-grammar behavior, so no shaky case was added. The "one pool, never
  a multiple" half of the rule is still checked indirectly by
  `red/ineligible-pool-does-not-fund-spend` (a single-topup source contributes exactly its
  100, not a doubled 200).

## Static validation cases

- `red/parameterless-balance-requires-eligibility-metadata` covers the SPEC §15.6 MUST that a
  parameterless accessor is invalid when `$eligible` references spend metadata; it is the one
  `suite: static` case in the area (outcome `invalid`).

## Spec gaps recorded as `outcome: unspecified`

- `red/negative-pool-quantity-enforcement-unspecified` — SPEC §15.1 requires pool
  `$quantity` to be non-negative but does not pin the *enforcement point* when the
  quantity is projected from a writable field: the insert of the offending row could
  be rejected, or the violation could surface only at meter evaluation. The outcome of
  inserting a topup whose projected `$quantity` is negative is therefore recorded as
  unspecified.

## Deliberate model choices

- The W3 flagship (`common/w3-overlapping-heterogeneous-credits`) and the overlapping
  red cases replace the spec example's calendar periods (`months`/`zone`) with fixed
  `P7D`/`P28D`/`P84D` periods bounded to a single occurrence via `ends_at`. This keeps
  every timestamp exact at seconds precision without depending on time-zone rule data,
  while preserving the ordering behavior (`$order: ["$until", ...]`, `none` last) that
  the case actually tests.
- `.credits.balance` is used in its parameterless form only where the meter has no
  `$eligible` referencing spend metadata (SPEC §15.6). `red/ineligible-pool-does-not-fund-spend`,
  whose `$eligible` reads spend `feature`, asserts through call outcomes instead of a
  parameterless balance view.
