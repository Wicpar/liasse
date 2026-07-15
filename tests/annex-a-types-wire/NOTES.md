# Annex A — Types and canonical wire values — chapter notes

Cases in this directory cover Annex A (Types and canonical wire values): the
primitive types and their canonical strict-JSON wire encodings (A.1), period
values (A.4), timestamp precision (A.5), decimal semantics (A.6), canonical
JSON (A.7), key-eligible types (A.8), and refs (A.9). They lean on the
supporting order rules in Annex B.1/B.3 and the canonical key text in Annex D.2
where wire identity is at stake.

Spec references use the `§A.x` / `§B.x` / `§D.x` section form already used
elsewhere in the corpus; the Annex A heading carries no explicit `<a id>`
anchor.

## Extension steps

None. Every case uses only the FORMAT.md step vocabulary (`watch` +
`expect_init` for reading wire forms, `call` for admission outcomes). Static
cases use `expect` on load.

## How wire-form expectations are asserted

The canonical wire value of a scalar is observed by projecting a field through
a `$view` and matching the returned JSON with the FORMAT.md matcher, which
distinguishes JSON types (string vs number vs boolean vs object). This is what
makes A.1 assertions non-tautological: the expected value follows from the
canonical-encoding rule, not from executing the program, and a wrong wire
*type* (e.g. int as JSON number) is a distinct value that fails the match.

## Spec gaps captured as `outcome: unspecified`

- **`decimal-canonical-trailing-zero-spelling-unspecified`** — A.1 says the
  decimal wire form uses "canonical sign and trailing-zero form" but never
  defines that form (strip trailing zeros? preserve operation scale?). The
  exact digit string of a value with trailing fractional zeros is not
  deducible. The 05-state-model `decimal-key-numeric-equality-collision` case
  independently flagged the same gap.
- **`uuid-uppercase-input-canonicalization-unspecified`** — A.1/D.2 pin the
  canonical *output* form of a uuid as lowercase hyphenated, but state no
  *input-acceptance* rule for a non-canonical (uppercase) uuid literal supplied
  as a key. Normalize-and-collide, reject-as-non-canonical, and accept-verbatim
  are all defensible. This gap is general: the same "canonical output form, no
  input rule" ambiguity applies to int leading zeros, decimal scale, duration
  spelling, and non-canonical base64. The uuid case is the concrete vehicle;
  the firm cases in this suite deliberately seed already-canonical inputs so
  they do not depend on the missing rule.

## Cross-corpus observation (not a case)

A.1 unambiguously encodes `int` as a "JSON string with canonical base-10
digits". The firm cases here (`int-wire-is-decimal-string`,
`int-wire-is-json-string-not-number`) assert the JSON *string* form, matching
`06-expressions/red/int-arithmetic-arbitrary-precision`. Note that
`05-state-model/common/composite-key-identity-and-lookup` instead expects a
bare JSON number (`rate: 20`, `value: 20`) for an `int` field. Per A.1 the
string form is canonical; the number form in that case is either a lenient
matcher assumption or a corpus inconsistency worth reconciling.

## Deliberately not covered here

- **A.4 calendar boundary arithmetic** (the "January 31, monthly, clamp" anchor
  sequence, `overflow`/`ambiguous`/`missing` policy behavior): observing period
  boundaries requires bucket/meter machinery (§14/§15) and a time-zone-aware
  clock oracle; it belongs to the buckets chapter corpus.
- **A.5 timestamp out-of-range diagnostic** ("A value exceeding the declared
  range produces a diagnostic"): the declared range is bounded by unstated
  implementation limits, so no concrete over-range value is deducible; left to
  a chapter that pins the limits.
- **A.5 precision-conversion arithmetic across mixed precisions**: the exact
  common-precision result is a §6 arithmetic concern; only the wire *type*
  (base-10 string) is asserted here.
- **A.6 division scale / rounding-mode results**: the numeric outcomes are
  governed by §6 and `$semantics`; `06-expressions` already carries
  `decimal-division-canonical-scale-unspecified` and the rounding cases.
- **A.7 canonical-JSON byte ordering of object keys**: the value matcher
  compares objects member-wise (order-insensitive), so on-the-wire key *byte
  order* is not observable through it. The observable consequence —
  member-order-invariant equality — is covered by
  `json-object-member-order-canonicalized-equal`.
