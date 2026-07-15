# Annex C — Grammar and syntax index — chapter corpus notes

Annex C is normative as a syntax index (SPEC.md: "This annex is normative as a
syntax index; detailed semantics remain in the feature chapters."). Every case
here therefore cites the Annex C form under test **plus** the feature chapter
that pins its semantics, so each expectation is deducible from SPEC.md text
alone.

## Extension steps

None. Every case uses only the step vocabulary defined in `tests/FORMAT.md`
(`watch` + `expect_init`, `call` + `expect`). No new step keys were introduced.

## Value conventions used by these cases

- Scalar `int` values in `$data`, `args`, and expected values use the canonical
  base-10 JSON string form (Annex A.1), e.g. `"100"`, matching the convention
  documented in `06-expressions/NOTES.md`.
- `decimal` results are avoided in expected values because Annex A does not pin
  a canonical result scale; `computed-field-equals-prefix-form` uses `int`
  arithmetic (exact per Annex A.5) so the canonical result is unambiguous.
- Set results are matched as arrays in element canonical order (§5.5: "Sets
  have canonical read order from the element type's total order"); for `text`
  members that order is the element total order, so `["a","b"]` is the read
  order of `{a,b}`.
- Non-ASCII characters that carry the point of a case (the Cyrillic confusable
  in `red/declaration-name-cyrillic-letter-invalid`) are described in the case
  comment by codepoint; the literal appears in the key so the built definition
  reproduces the exact scalar.

## Readings adopted

- **Escape generality (Annex C.4).** §4.2 states only the `=`-prefixed literal
  escape (`"'= text"`). Annex C.4 generalizes to any literal: `"'text"` =
  "literal text with one leading `'` removed". The corpus treats the Annex C.4
  form as normative for all literal-or-expression positions, so
  `common/literal-leading-quote-removed-in-data`,
  `red/double-leading-quote-stores-single-quote`, and
  `red/lone-quote-in-data-stores-empty-string` deduce their stored text from
  the single-quote-removal rule directly.
  `common/literal-equals-prefixed-text-escaped-in-data` is the §4.2-documented
  case proper (its own `"'= total + tax"` example): the escape both removes the
  leading `'` and suppresses expression evaluation of the `=`-prefixed body.
- **Exactly one package identity (Annex C.1).** Annex C.1 states "Exactly one
  of `$app` and `$module` identifies a package"; §4.3 treats the two as the
  distinct application vs. module package kinds.
  `red/package-declares-both-app-and-module-invalid` encodes the both-declared
  violation of that exactly-one rule.
- **Always- vs literal-or-expression positions (Annex C.4).** `$check`,
  `$view`, `$normalize`, selector filters, projections, and mutation statements
  are always-expression positions, so a plain string there is an expression, not
  a literal. `common/always-expression-position-parses-string-as-expression`
  encodes this by placing an unresolvable field reference in `$check` and
  requiring load-time rejection.
- **Wildcard absence (Annex C.6).** "A wildcard selection syntax is absent;
  projections name fields explicitly." Encoded twice, for the selector position
  (`red/wildcard-selector-star-invalid`) and the projection position
  (`red/wildcard-projection-star-invalid`).

## Spec gaps recorded as `outcome: unspecified`

1. `red/bare-equals-empty-expression-in-data-unspecified` — Annex C.4 / §4.2
   define `"= expr"` for a non-empty expression but leave the degenerate value
   `"="` (marker, empty body) undefined: error vs. literal `=` vs. empty result.
2. `red/conflicting-key-and-set-markers-unspecified` — Annex C.2 defines the
   zero-marker case (static struct) and each marker in isolation (§5.4 `$key`,
   §5.5 `$set`) but never states the meaning or validity of an object bearing
   two mutually exclusive shape markers.
3. `red/combinators-lack-precedence-and-grouping-unspecified` — Annex C.8 / §7.4
   list the view combinators but define no precedence, associativity, or
   grouping-parenthesis syntax, so a mixed chain `.a | .b & .c` has no deducible
   grouping (or well-formedness).
4. `red/empty-mutation-program-array-unspecified` — Annex C.9 / §8.1 define
   one-statement and multi-statement programs but not the empty statement array
   (no-op valid program vs. static error).
