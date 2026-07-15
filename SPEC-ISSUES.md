# SPEC-ISSUES — ambiguities surfaced by the conformance corpus

Writing the `tests/` corpus (every expectation re-derived from SPEC.md text
alone) surfaced places where the v0.5 spec does not pin observable behavior.
Each item below is carried by one or more corpus cases with
`outcome: unspecified`; resolving an item means amending SPEC.md and then
tightening those cases to the pinned outcome. Until resolved, the
implementation must not silently pick a side.

Case references are `area/case-name` under `tests/` (reds unless noted).

## Values and expressions

1. **Canonical decimal wire spelling.** Annex A.1 ("canonical trailing-zero
   form") plus A.6 ("at least sixteen fractional digits") pin no single wire
   text for decimals with variable fractional zeros — hits division results,
   every `avg` (§7.5), and decimal keys.
   `06/decimal-division-canonical-scale`, `annex-a/decimal-canonical-trailing-zero-spelling`,
   `05/decimal-key-numeric-equality-collision`, `07/synthetic-key-numeric-equal-merges-groups`.
2. **Non-canonical input acceptance.** The spec pins canonical *output* forms
   (uuid lowercase, int without leading zeros, canonical base64/duration) but
   no input rule: normalize-and-collide vs reject vs accept-verbatim.
   `annex-a/uuid-uppercase-input-canonicalization`.
3. **Arithmetic edges.** `%` remainder existence and negative-operand sign;
   division by zero; arithmetic over an optional operand (static type error vs
   `none` propagation). `06/integer-remainder-sign`, `06/division-by-zero`,
   `06/optional-operand-arithmetic`.
4. **Generated-value identity per call-site.** §5.1/§8.12 "produced once for
   the admitted request" read literally makes two `uuid()` key defaults in one
   request collide; Annex A.5 disambiguates `now()` but nothing disambiguates
   `uuid()`. `06/generated-value-per-call-site`.
5. **`string.trim` and non-ASCII whitespace.** Does a U+00A0-only title
   normalize to empty (and fail `size(.) > 0`)?
   `w-worked-examples/w1-title-unicode-whitespace-trim`.

## Mutations, calls, and surfaces

6. **Unknown members in call arguments.** Reject vs ignore is unstated;
   ignoring also makes §12.3's "equivalent request" dedup ill-defined.
   `08/unknown-argument-name`, `12/unknown-parameter-member`.
7. **Delete-by-key of an absent key.** §8.9 rejects an absent keyed patch but
   lets zero-match filtered ops succeed unchanged; `collection - key` is
   unassigned. `08/delete-absent-key-outcome`.
8. **Failure class for unresolvable/ungranted names.** §10.4/§12.1 mandate
   failure but pin no category (denied vs not-found), and don't require a
   nonexistent surface to be indistinguishable from an ungranted one
   (enumeration leak). `10/unresolvable-name`,
   `11/public-surface-authenticator-selection`.
9. **`$actor` with no actor bound.** Public request and host-operator
   transition: static reject (§6.2) vs admission reject (§6.3) vs null
   binding; whether host provenance is application-readable is also unpinned.
   `22/public-request-references-actor`, `23/operator-commit-actor-provenance`.
10. **Surface `$view` parameter inference.** Inference is defined for
    mutations only (§8.3). `06/surface-view-parameter-inference`.
11. **Interface addressing edges.** Recursive-descendant mutation addressing;
    `$where`/`$except` excluded-branch representation; empty surface
    declaration. `10/recursive-descendant-mutation-addressing`,
    `10/where-excluded-branch`, `10/empty-surface-declaration`.

## Views

12. **Synthetic `$key` constraint scope.** §7.2 states the
    "aggregated or key-derived" constraint unconditionally yet allows a
    synthetic key "for grouping OR a new identity"; application to a per-row
    unique identity is undisambiguated.
    `07/synthetic-key-new-identity-nonaggregated`.

## Buckets, meters, timing

13. **Enforcement points.** Negative projected pool `$quantity`: reject at
    insert vs at meter evaluation. Calendar `overflow: reject`: eager at the
    source transition vs lazy at the temporal read. Connector resolution
    timing. `15/negative-pool-quantity-enforcement`,
    `14/overflow-reject-detection-timing`, `18/connector-resolution-timing`.
14. **Funding-row projected metadata.** Whether a source-projected member
    (e.g. `price`) appears in each §15.3 funding row.
    `w-worked-examples/w3-funding-projected-metadata-shape`.

## Host components

15. **Nonconforming components.** Runtime handling when a registered component
    returns an off-contract type, or a declared-`pure` function returns
    divergent values across evaluations/replay — §2.1/§16.2 assume conformance.
    `23/namespace-returns-off-contract-type`,
    `23/impure-pure-function-replay-divergence`, `16/namespace-type-key-eligibility`.
16. **Budgets and hangs.** §23.6 admits backpressure or rejection "per the
    declared API contract" — with no declared policy, unpinned; with no
    mutation-time budget, a hanging component has no mandated timeout.
    `23/budget-backpressure-or-reject-choice`, `23/no-time-budget-hanging-provider`.
17. **Namespace/requirement resolution.** Name-vs-contract resolution; a
    `$requires` key colliding with a model name; unused requirement
    declarations. `09/namespace-resolution-name-vs-contract`,
    `16/requires-key-collides-with-model-name`, `16/unused-requirement-declaration`.

## Keyrings

18. **Rotation timing and metadata.** Retain-boundary instant; overlap
    exceeding cadence; pending-version acceptance during overlap; whether the
    usage set excludes the call-site operation; §17.2 pins no version-metadata
    member names. `17/retain-boundary-instant`,
    `17/rotation-overlap-exceeding-cadence`, `17/pending-version-acceptance`,
    `17/usage-excluding-callsite`.

## Blobs

19. **Recovery past a lying holder.** Fetch recovery from an honest holder is
    a client MAY, so the single-fetch outcome is unpinned even when an honest
    holder exists. `18/all-holders-corrupt-fetch-outcome`,
    `23/connector-tampered-read-refetched-from-verified-holder`.
20. **Wire details.** Descriptor bytes encoding; uppercase-hex sha512
    handling. `18/descriptor-bytes-encoding`, `18/uppercase-hex-sha512`.

## History, artifacts, evolution

21. **Reconciliation identity.** Merge of equal inserts and row incarnation;
    point-id aliasing across unrelated histories; forged state with
    self-consistent checksums; unknown extra artifact entries; manifest
    included-range statement. `19/merge-equal-inserts-incarnation`,
    `19/point-id-aliasing`, `19/forged-state-consistent-checksums`,
    `annex-d/liasse-json-swap-with-fixed-checksums-stale-identity`,
    `19/unknown-extra-entry`, `19/manifest-included-range`.
22. **Update/downgrade edges.** Target package's own `$data` seed on a
    root-app update (§13.13's three-way merge is module-only); version-sequence
    composition is a MAY; same-version republish that widens the contract;
    shape-compatible downgrade with no explicit transform.
    `20/target-seed-data-on-update`, `20/sequence-composition-not-mandated`,
    `annex-e/same-version-republish-widened-contract`,
    `annex-e/downgrade-shape-compatible-no-transform`.
23. **Seed-time semantics.** Seeded-default sibling visibility vs prospective
    state; re-evaluation of a stored field seeded from a cross-instance
    expression; reload with divergent seed data.
    `05/seed-default-sibling-visibility`,
    `w-worked-examples/w4-seed-computed-enabled-reevaluation`,
    `09/reload-divergent-seed-data`.
24. **Erasure scope.** Scrub scope over a cascade-deleted row's retained
    history. `21/erase-cascade-scrub-scope`.

## Grammar (Annex C)

25. **Degenerate forms.** Bare `"="` (marker with empty expression body); one
    object bearing both `$key` and `$set`; view-combinator
    precedence/associativity/grouping; empty mutation-program array.
    `annex-c/bare-equals-empty-expression-in-data`,
    `annex-c/conflicting-key-and-set-markers`,
    `annex-c/combinators-lack-precedence-and-grouping`,
    `annex-c/empty-mutation-program-array`.

## Package identity (§4, Annex D/E)

26. **Name grammar edges.** Prerelease `$app` version; single-component
    package name; resource `$path` outside `resources/`; ref-to-nested-
    collection key form. `04/app-version-prerelease`,
    `04/single-component-package-name`, `04/resource-path-outside-resources`,
    `05/ref-to-nested-collection-key-form`.

## Runtime

27. **Clock and ordering fine print.** `now()` half-unit precision tie-break;
    cross-connection wall-clock causality as serial precedence is only a MAY.
    `22/now-half-precision-rounding-mode`, `22/cross-connection-sequential-order`.
