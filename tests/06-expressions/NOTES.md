# §6 Expressions — chapter corpus notes

## Extension steps

None. Every case in this chapter uses only the step vocabulary defined in
`tests/FORMAT.md` (`call`, `watch`, `expect_view`, `advance_time`).

## Value conventions used by these cases

- Typed scalar values in `args`, `$data`, and expected values use the
  canonical strict-JSON wire forms of SPEC.md Annex A.1: `int`, `decimal`,
  and `timestamp` as canonical base-10 JSON strings, `bool` as `true`/`false`,
  `text` as JSON strings preserved exactly.
- A view result is an array of row objects; a single-row value (for example a
  one-row insertion bound and returned by a mutation) is one object; a surface
  `$view` projecting the model root (`. { ... }`) yields one object.
- Seed map keys in `$data` use Annex D.2 canonical key text; composite keys
  join their components with `:` in `$key` order (`"fr:std"`).
- The harness virtual clock (FORMAT.md determinism rules) is the host clock
  sampled by `now()`: 2026-01-01T00:00:00Z = `1767225600000000` at the default
  `us` precision.

## Readings adopted where the spec has two adjacent rules

- §6.3 distinguishes one-row contexts from bulk writes. A row-mutation
  *receiver* "MUST receive exactly one occurrence; zero or several occurrences
  reject", while *mutation writes* "deduplicate selected target rows by row
  incarnation". The corpus encodes both: a duplicate-key receiver rejects
  (`red/row-mutation-receiver-duplicate-occurrences-reject`) while a
  duplicate-key bulk patch applies once
  (`red/patch-duplicate-selector-applies-once`).

- §7.5 has two clauses about absence: "Absent inputs are skipped" and empty
  input yields "none for avg, min, and max". The corpus reads the `none`
  clause as applying to *empty* input only: a non-empty input that merely
  contains absences skips them and still returns a present result
  (`common/aggregate-skips-absent-inputs`, min over {10, none, 20} = 10),
  while a genuinely empty input yields absence
  (`common/aggregate-empty-input-identities`, max over {} absent).

## Spec gaps recorded as `outcome: unspecified`

1. `red/integer-remainder-sign-unspecified` — §6.1 adopts CEL *syntax* only;
   Annex A.6 pins division but not `%` (existence or negative-operand sign).
2. `red/division-by-zero-unspecified` — no rule anywhere defines a zero
   divisor's behavior.
3. `red/decimal-division-canonical-scale-unspecified` — Annex A.6 "at least
   sixteen significant fractional digits" + Annex A.1 "canonical ...
   trailing-zero form" do not pin one observable wire text for `10 / 4`
   (also affects every `avg` result, §7.5).
4. `red/optional-operand-arithmetic-unspecified` — no rule states whether an
   arithmetic operator over an optional operand is a static type error or
   propagates `none`.
5. `red/generated-value-per-call-site-unspecified` — §5.1/§8.12 "produced
   once for the admitted insertion/request" read literally makes two `uuid()`
   key defaults in one request collide; Annex A.5 disambiguates `now()` but
   nothing disambiguates `uuid()`.
6. `red/surface-view-parameter-inference-unspecified` — parameter inference
   is defined for mutations (§8.3); whether a surface `$view` may use `@name`
   without a `$params` declaration is not pinned.
