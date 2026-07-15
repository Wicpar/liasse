# Annex E — Package compatibility — chapter notes

Cases in this directory exercise SPEC.md Annex E (anchor `#annex-e`): the
version rule (E.1), boundary contracts (E.2), mechanical/semantic checking
(E.3), input compatibility (E.4), output compatibility (E.5), private model
evolution (E.6), mutation contracts (E.7), namespace/host-capability
compatibility (E.8), and update diagnostics (E.9). Annex E is the full
algorithm behind the summary rules in §20.3 (`#evolution`) and §13.14
(`#modules`), so cases cross-cite those anchors where the same rule is stated
normatively there.

Annex E is *comparative*: a release is compatible or breaking only relative to
an earlier release. Every case therefore establishes a baseline release, then
loads or updates to a candidate release and asserts the outcome the earlier
contract mandates for the candidate.

## Extension steps

Annex E's checks run "before activation" at two host lifecycle operations:
package `load`/update (E.1, E.9; §9.2) and single-instance module update
(E.1; §13.14/§13.15). FORMAT.md has no vocabulary for either, so this chapter
reuses the extension steps already defined by the loading and modules chapters,
with identical semantics:

| step | semantics |
|---|---|
| `host_load` | `{ host_load: { package: { ...definition... } }, expect: {...} }`. The harness builds a `.liasse` artifact from the inline definition and invokes the host `load` operation against the case's root instance (§9.2). It runs outside any client connection and carries no actor, role, or credential. On `expect.outcome: ok`, member `result` states the §9.4 lifecycle outcome and MUST be `committed` or `unchanged`. See `../09-loading-bootstrap/NOTES.md`. |
| `module_install` | `{ module_install: { space: "<display path>", request: { $name, $module } }, expect }`. §13.3 install into a module space; `$module` resolves against the case's `packages` map by each entry's `$module`. See `../13-modules/NOTES.md`. |
| `module_update` | `{ module_update: { instance: "<display path>", to: "name@version" }, expect }`. §13.14/§13.15 single-instance update; `to` resolves against the `packages` map. On success `expect.value` matches the §13.15 update report. See `../13-modules/NOTES.md`. |

## Outcome mapping for compatibility results

Annex E's own language is "reject": E.3 "It MUST reject every narrowing"; E.9
"A rejected load or update leaves the current package, bindings, and state
active." §13.14/§20.3 speak of loading/publication that "reject a narrowing
release." Mapping onto FORMAT.md's vocabulary:

- `rejected` — the candidate definition is itself well-formed and would load on
  its own, but it **narrows** a boundary contract relative to an earlier
  release in the same major (E.1, E.3, E.4, E.5, E.7, E.8), or a downgrade
  cannot represent current live values (§20.2). This matches FORMAT.md's
  `rejected` = "admission-time rejection (checks, keys, refs, uniqueness,
  meters, limits)": a compatibility narrowing is a boundary check refused at
  admission of the update, and it is the same single §9.4/§13.15 `rejected`
  lifecycle result. This is the classification used by
  `../13-modules/common/minor-update-narrowing-rejected.hjson`, followed here.
- `invalid` — the candidate is *statically* bad on its own terms, independent
  of any earlier release: an unsupported `$liasse` generation (§4.1, the runtime
  MUST reject it before interpreting other members) or an illegal declaration
  name (§2.5). These fail before any Annex E comparison runs.

Where a case's split between `rejected` and `invalid` could be argued, its
`note` says so; the non-ok outcome itself is never in doubt.

## `unspecified` cases

Two red cases record genuine gaps as `outcome: unspecified` (FORMAT.md requires
a `note`):

- `same-version-republish-widened-contract-unspecified` — whether load accepts a
  same-`$app`-version definition that *widens* (does not narrow) its boundary
  contract. E.1 forbids only narrowing; the text does not say whether a
  same-version, different-definition republish is a no-op, a commit, or an
  identity error.
- `downgrade-shape-compatible-no-transform-unspecified` — whether a downgrade
  that loses no live values but declares no explicit transform commits via the
  §20.1 compatible auto-copy, or is rejected because §20.2 phrases a downgrade
  as "applies an explicit direct migration or available exact inverses."

Note on `$liasse`: an attempt to switch generation across a minor is NOT
unspecified — only generation `1` exists in v0.5, so any other value is
statically rejected as unsupported (§4.1). That deducible outcome is the
`invalid` red case `minor-changes-liasse-to-unsupported-generation-rejected`;
no separate unspecified `$liasse` case exists because the concrete outcome is
pinned.

## Enum domain: input vs output asymmetry

An enum is a *type* (§5.9), not a `$check`, so a mutation parameter inferred
from an enum field inherits the enum's accepted-label set as part of its type
(§8.3: "The resulting parameter shape is part of the external surface
contract"). Annex E therefore treats the same structural change oppositely on
each side of a boundary:

- Input (parameter) domain — widening is compatible (E.4 "widening an ...
  accepted enum domain"), narrowing is breaking (E.4 "narrowing a parameter
  check or accepted enum domain"). Cases:
  `minor-widens-input-enum-domain-accepted`,
  `minor-narrows-input-enum-domain-rejected`.
- Output (result) domain — narrowing/removal is breaking (E.5 "removing or
  renaming an output member") and *widening an exhaustively declared enum
  result* is also breaking (E.5). The red case
  `enum-result-confusable-label-swap-rejected` exercises the output side.

The input cases keep the enum field out of the projected view so the change
touches only the input contract; the output case keeps it out of any parameter.

## Identity: declaration names vs label values

The §2.5 ASCII-only rule constrains *declaration names* (surface addresses,
field names). Enum labels (§5.9) are checked text *values*, not declaration
names, so their identity is by codepoint under canonical text (Annex D). Two
red cases exploit this split:

- `confusable-surface-address-rename-rejected` — a homoglyph surface *name*
  (Cyrillic `е`, U+0435) is an illegal non-ASCII declaration name (§2.5): the
  candidate is statically `invalid`, so a boundary address can never be
  confused.
- `enum-result-confusable-label-swap-rejected` — a homoglyph enum *label*
  (`cIosed`, capital I U+0049, for `closed`) is a valid ASCII label, so the
  definition builds, but it is a distinct value under canonical text; the
  exhaustive output enum domain loses the promised label `closed`, which E.5
  makes a breaking output change (`rejected`).
