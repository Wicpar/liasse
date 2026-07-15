# §14 Buckets — corpus notes

## Step-vocabulary extensions

No new step *keys* are introduced. One existing step is extended:

- **`watch` accepts an optional `args` member** — typed parameter values for the
  watched surface's `$params` (SPEC §10.1, §12.1 `view` operation). FORMAT.md
  shows `watch` without arguments; parameterized temporal views (`.$at(@t)`,
  `.$between(@a, @b)`) cannot be exercised without it.
  Example: `{ watch: "public.periods_at", args: { t: "1767830400000000" }, id: "w1", expect_init: {...} }`

No new step *keys* beyond the extension above are introduced. `restart: {}`
(used by `red/restart-preserves-recorded-created-interval`) is a FORMAT.md
built-in, and the temporal reads use `$at`/`$between` selectors inside ordinary
`call`/`watch` steps.

## Spec ambiguities captured (`outcome: unspecified`)

- **`red/overflow-reject-detection-timing-unspecified`** — A.4 defines the
  `overflow: reject` policy (reject when a generated calendar boundary lands on
  a date missing from the destination month), but neither §14.5 nor A.4 fixes
  *when* that rejection surfaces: eagerly at the source-row transition, or
  lazily only when a temporal read requires the missing boundary. §14.5's
  explicit "rejects the source row or the transition that produced it" is worded
  for non-advancing recurrence, not for a mid-series missing calendar date. The
  case asserts `outcome: unspecified` for the admission of a subscription whose
  monthly series (anchored Jan 31) would require Feb 31.

## Harness-semantics assumptions (documented, not spec extensions)

- **`advance_time` establishes the temporal observation before the next step.**
  §22.6 requires the runtime to reflect the resulting current logical view and
  emit a new live frontier once a temporal observation is established; §12.2
  says a frontier covers committed changes *and* temporal bucket observations.
  Cases therefore use `expect_view` after `advance_time` to assert the view
  including the rows that entered/left their active interval at the new virtual
  instant.
- **`restart` preserves the virtual clock.** FORMAT.md pins the clock to
  `2026-01-01T00:00:00Z` and says only `advance_time` moves it; replay of
  durable state (§22.1 recorded observations) must not move it either.

## Wire-value conventions (Annex A)

- `timestamp` values in `args` and expectations are canonical wire values:
  base-10 strings counting **microseconds** since the epoch (package default
  precision `us`, Annex A.5). The virtual start instant
  2026-01-01T00:00:00Z = `"1767225600000000"`. Case comments give the ISO form.
- `int` and `decimal` values are canonical digit strings (Annex A.1), so a
  projected `$index` of zero is `"0"` and a decimal credit of 30 is `"30"`.
- `none` is **absence**: an expected row omits the member entirely (FORMAT
  exact-object matching) — used for unbounded `$until`. JSON `null` is never a
  valid optional-timestamp value (Annex A.1).

## Outcome mapping

- Spec phrases "rejects evaluation" (§14.1 `$between`) and "rejects the source
  row or the transition that produced it" (§14.5) are asserted as
  `outcome: rejected` (admission/request-time rejection with diagnostics).
- Read-only calls implemented as mutations ending in `return` produce no state
  change and complete with the `unchanged` status carrying the evaluated
  response (§8.9, §12.3). The corpus asserts these as `outcome: ok` with the
  returned `value`, since `unchanged` is a success status.

## Deliberately uncovered rules (with reasons)

- **§14.8 spend partitioning** ("Each entry is assigned to the active exercise
  at its `$time`"): observing the assignment requires §15 meter machinery
  (funding records / meter accessors); it belongs to the meters chapter corpus.
  The bucket-side rule it relies on — which interval is active at a given
  instant — is covered here by the `$at` boundary cases.
- **§14.6 "The source identity includes its complete source row chain.
  Composite components are flattened in order."** — externally addressing a
  derived row by its *inferred* composite identity is itself not defined by the
  spec (selector composite lookups name components, §6.3, but inferred identity
  components have no declared names), so no expectation about the flattened
  form is externally deducible. Custom-key behavior (§14.6) is covered instead.
- **§14.3 meter `$order`** — the normative sentence is about the ordinary total
  order of optional values; it is asserted through view `$sort` (§7.3/Annex B.2).
  Meter pool-ordering behavior belongs to the §15 corpus.
