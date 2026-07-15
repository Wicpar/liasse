# Annex B — Deterministic total order — corpus notes

All cases in this chapter exercise **Annex B** (normative) using the smallest
possible vehicle: a keyed collection seeded with `$data`, a public `$view` that
either declares a `$sort` over the field under test or relies on the default
key-ascending order, and a `watch` whose `expect_init` asserts the **ordered**
result array. Because `expect_init.value` is an ordered array (not a
`{ $unordered: [...] }` set), the row order is what is being asserted — that is
the whole point of the chapter.

Nothing here executes mutations except where a case explicitly needs a second
observation (`restart`). Order is a pure function of values and identities
(Annex B), so a seed + view is sufficient to pin it.

## Step vocabulary

No new step keys are introduced. Cases use only `watch` / `expect_init`,
`unwatch`, and `restart`, all defined in [FORMAT.md](../FORMAT.md).

One **expectation-shape extension** is reused from the §5 corpus:

- `expect_init: { outcome: unspecified }` — on a `watch` step, marks a read
  whose ordering the spec does not pin, while package load and the watch
  itself succeed. The harness records the observed order without judging, as
  it does for a case-level `outcome: unspecified`. Used only by
  `red/struct-field-name-order-unspecified.hjson`; its `note` explains the gap
  (§B.4 / §D.2 use the term "canonical field-name order" without defining it
  when it diverges from declaration order).

## Authoring conventions

- **Scalar wire forms.** Values are authored in the canonical wire forms of
  Annex A, matching the rest of the corpus:
  - `int` — plain Hjson numbers in `$data` values and expected values (canonical
    base-10 per A.1); negatives written directly (`-10`).
  - `decimal` — strings (`"1.00"`), because Annex A does not pin the canonical
    trailing-zero spelling for assertion. The decimal case therefore projects
    only `id` and never asserts a decimal's rendered spelling — only row order.
  - `bytes` — `{ "$bytes": "<base64>" }` (A.1); each byte value's base64 is
    spelled out in the case note.
  - `uuid` — lowercase hyphenated strings (A.1).
  - `json` — literal Hjson `null` / bool / number / string / array / object.
    A `json?` field seeded by omission is `none`; seeded as `null` it is the
    JSON null value. These are different values that sort at opposite ends
    (see below).
- **Unicode.** Case files are UTF-8. `red/text-order-is-scalar-value-not-utf16`
  embeds literal astral and high-BMP characters; every codepoint is spelled out
  (U+007A, U+FFFD, U+10000) in the note so a mangled file is detectable.
- **Seed order is deliberately scrambled** relative to the expected output in
  every case, so that a run reproducing insertion order rather than value order
  fails.

## Deduction notes for non-obvious expectations

- `common/descending-reverses-with-none-first` — under a single descending key
  `-v` the two `none` rows tie and fall to the final identity tiebreaker, which
  §B.5 defines as row/occurrence identity **ascending** (the example in §B.5
  appends "occurrence identity ascending as the final tiebreaker" even though
  its leading key is descending). So `none` rows lead (per §B.2) and are ordered
  n1 before n2 (key ascending), then the present block follows descending.
- `red/json-null-ranks-first-optional-none-ranks-last` — §B.3 ranks JSON `null`
  lowest inside `json`, while the `json?` optional wrapper places `none` last
  ascending (§B.2/§B.3). A.1 and §7.3 state JSON `null` and `none` are distinct
  values. The case asserts they occupy the first and last positions
  respectively.
- `red/bytes-unsigned-byte-order` — §B.1 says *unsigned* byte order, so 0x80 and
  0xFF sort above 0x00/0x7F; the prefix pair `[0x41] < [0x41,0x00]` uses the
  lexicographic "shorter after shared prefix" behaviour, consistent with the
  array rule in §B.3.
- `red/composite-key-second-component-uses-int-order` — §B.4 compares composite
  keys component-wise in `$key` order, each in its own §B.1 type order, so the
  `int` second component uses numeric (2 < 10), not the text order of the
  D.2-joined key string.
- `red/struct-field-name-order-unspecified` — genuine ambiguity: "canonical
  field-name order" (§B.4, §D.2) is undefined when declaration order differs
  from field-name text order. The struct is declared `(b, a)`, the reverse of
  text order, so the two readings disagree on the row order; recorded as
  `unspecified`.

## Sorting by non-scalar fields

Several cases sort a view by a `json`, `bytes`, `set`, `ref`, or `struct`
field. This is deducible: Annex B defines a total order for each of those types
(B.1, B.3, B.4), and `$sort` compares the declared sort expressions using the
value order (B.5). The annex would not define an order for a type that could
never be a sort key.
