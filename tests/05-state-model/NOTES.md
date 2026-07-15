# §5 State model — corpus notes

## Step vocabulary

No new step keys are introduced; every case uses `call`, `watch`, and
`expect_view` exactly as defined in [FORMAT.md](../FORMAT.md).

One **expectation-shape extension** is used:

- `expect_init: { outcome: unspecified }` — on a `watch` step, marks a read
  whose *value* the spec does not pin, while the package load and the watch
  itself succeed. The harness records the observed value without judging, as
  it does for case-level `outcome: unspecified`. Used only by
  `red/seed-default-sibling-visibility-unspecified.hjson`; the case `note`
  explains the ambiguity (§5.1 bulk-statement selectability vs. §9.1 seed
  prospective state).

## Authoring conventions

- **Numeric values.** `int` and `bool` values in `args` and expected values
  are authored as plain Hjson numbers/booleans; they denote the typed Liasse
  values whose canonical wire forms are defined in Annex A (`int` travels as
  a canonical base-10 JSON string). `decimal` values are authored as strings
  (`"1.50"`), matching their Annex A wire form, because Annex A does not pin
  the canonical trailing-zero spelling and cases must not assert it.
- **Unicode.** Case files are UTF-8 and embed literal non-ASCII scalars where
  the rule under test is scalar-exact identity
  (`red/unicode-confusable-keys-are-distinct.hjson`,
  `red/non-ascii-declaration-name-invalid.hjson`). The codepoints involved
  are spelled out in each case's `note` so a mangled file is detectable.
- **Read-only calls.** Several cases call a mutation whose program is a
  single `return` (e.g. `get`). Per §8.9 such a call produces no commit and
  is delivered with the `unchanged` status; the cases assert only
  `outcome: ok` plus the returned value and never assert the
  committed/unchanged status distinction.
- **Absent members.** Expected objects rely on FORMAT.md exact matching;
  `"$absent"` is used where absence *is* the rule under test, plain omission
  otherwise.

## Deduction notes for non-obvious expectations

- `red/empty-text-key-is-a-real-key.hjson` — `text` is a "Unicode scalar
  sequence, preserved exactly" (Annex A.1), which includes the empty
  sequence; `text` is key-eligible (A.8); no rule in §5 imposes a minimum
  key length. Rejecting `""` would add a constraint the spec does not state.
- `red/decimal-key-numeric-equality-collision.hjson` — asserts only the
  collision (B.1: numerically equal canonical decimals compare equal), never
  the canonical decimal spelling.
- `red/normalization-defeats-unique-evasion.hjson` — §9.1 lists the
  admission order "defaults, normalization, checks, key, ref, uniqueness";
  uniqueness therefore observes normalized field values.
