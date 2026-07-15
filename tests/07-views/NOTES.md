# §7 Views — chapter corpus notes

## Extension steps

None. Every case uses only the step vocabulary defined in `tests/FORMAT.md`
(`watch` + `expect_init`). All view results are asserted at load time from
seeded `$data`, so no `call`/`advance_time`/`restart` is needed to isolate the
read semantics of §7.

## Value conventions used by these cases

Same as the corpus-wide conventions (see `tests/06-expressions/NOTES.md`):

- Typed scalars in `$data` and expected values use the canonical strict-JSON
  wire forms of Annex A.1: `int`/`decimal` as base-10 JSON strings, `bool` as
  `true`/`false`, `text` preserved exactly.
- A view result is an array of row objects; a surface `$view` projecting the
  model root (`. { ... }`) yields one object. Aggregate computed values are
  exposed by projecting the root so a single object carries them.
- Seed map keys in `$data` use Annex D.2 canonical key text.
- Expected arrays are written in the exact order the view must produce; order
  is load-bearing for every `$sort`, bounds, and combinator case. Set-valued
  outputs (`distinct`) are written in the element type's canonical order
  (Annex B), which the spec fixes, so no `$unordered` is used.

## §7.5 aggregate coverage

- **Empty input.** `red/aggregate-empty-input-zero-and-none` exercises the whole
  §7.5 empty-input rule at once on an unseeded (empty) collection: `count -> 0`,
  `sum -> 0`, and `avg`/`min`/`max -> none`.
- **avg decimal division.** `common/aggregate-avg-converts-to-decimal` pins that
  `avg` converts int inputs exactly to `decimal` and divides (1,2 -> "1.5"),
  never integer-truncating.
- **Skip absent.** `red/aggregate-min-skips-absent-inputs` covers "Absent inputs
  are skipped" with at least one present value remaining.

## Citation of the `none` wire form

An omitted optional field is `none` (Annex A.1: "absent `optional<T>` value ...
omitted optional field"), which renders as an absent member matched with
`$absent`. Cases asserting an absent optional in a result cite **§A.1** for the
representation (plus §B.2/§B.3 for where `none` sorts), rather than §5.2, whose
`none` clause is about *computed* expressions yielding `none`.

## Readings adopted where the spec has two adjacent rules

- **§7.2 grouping vs. §7.5 aggregates.** A synthetic `$key` output row's
  non-key values are constrained by §7.2 ("MUST be aggregated or derived solely
  from key values"). `common/synthetic-key-groups-rows-with-aggregate` and
  `common/composite-synthetic-key-orders-components` emit only key fields plus
  `sum(group.debit)`; `red/synthetic-key-nonaggregated-value-invalid` rejects a
  bare non-key output.
- **§7.4 combinators, "left projection and order".** `&`, `-`, and the union's
  left-order rule are each asserted against the LEFT operand's projection and
  ordering (`common/intersection-keeps-left-projection`,
  `common/difference-removes-right-rows`,
  `common/union-concatenates-left-then-new-right`).
- **§7.3 / Annex B.5 tiebreak.** In a keyed collection every occurrence has a
  distinct row key, so "occurrence identity, the final tiebreaker" observes as
  row-key ascending among equal sort keys
  (`common/sort-tiebreak-by-row-identity`).
- **none vs. JSON null (§7.3, Annex B.3).** `none` is represented as an omitted
  `$data` member of an optional field (§5.2 → absent in the result, matched with
  `$absent`); a stored JSON null value is written as a literal `null` on a
  `json` field. `red/sort-none-distinct-from-json-null` asserts the two are
  distinct and sort at opposite ends (null lowest, none last, ascending).

## Spec gaps recorded as `outcome: unspecified`

1. `red/synthetic-key-new-identity-nonaggregated-unspecified` — §7.2 allows a
   synthetic `$key` "for grouping OR a new identity" but states the "aggregated
   or key-derived" constraint unconditionally. Whether that constraint applies
   when the synthetic key is a per-row unique identity (no rows share it) is not
   disambiguated.
2. Not recorded as a separate case here: the canonical decimal wire form chosen
   to represent a group whose key was supplied in numerically-equal but
   textually-different forms (`1.0` vs `1.00`). Annex A.1's canonical
   trailing-zero form does not pin which input representative survives, so
   `red/synthetic-key-numeric-equal-merges-groups` matches the merged `bucket`
   output with `$any` and asserts only the merge (group count and summed
   totals). This is the same decimal-canonicalization gap already logged in
   `tests/06-expressions/NOTES.md` (item 3).
