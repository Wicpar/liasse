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
   **RESOLVED (pending SPEC.md merge):** Annex A.6 amended; corpus pinned
   (remainder-sign now a scenario asserting `-1`, division-by-zero → `rejected`,
   optional-operand → `invalid`).
4. **Generated-value identity per call-site.** §5.1/§8.12 "produced once for
   the admitted request" read literally makes two `uuid()` key defaults in one
   request collide; Annex A.5 disambiguates `now()` but nothing disambiguates
   `uuid()`. `06/generated-value-per-call-site`.
   **RESOLVED (pending SPEC.md merge):** §5.1/§8.12/§16.3 amended and Annex A.5
   cross-referenced — `uuid()` yields a fresh, distinct value per evaluation
   (per row for one field-default call site); "produced once" is a
   recording/replay guarantee; `now()` stays the shared request instant. Runtime
   mixes a per-row generation ordinal into the `uuid()` derivation; the corpus
   case is now a scenario asserting `count == 2`.
5. **`string.trim` and non-ASCII whitespace.** Does a U+00A0-only title
   normalize to empty (and fail `size(.) > 0`)?
   `w-worked-examples/w1-title-unicode-whitespace-trim`.
   **RESOLVED (pending SPEC.md merge):** §6.5 amended (Unicode `White_Space`);
   W1 case pinned → `rejected`.

## Mutations, calls, and surfaces

6. **Unknown members in call arguments.** Reject vs ignore is unstated;
   ignoring also makes §12.3's "equivalent request" dedup ill-defined.
   `08/unknown-argument-name`, `12/unknown-parameter-member`.
7. **Delete-by-key of an absent key.** §8.9 rejects an absent keyed patch but
   lets zero-match filtered ops succeed unchanged; `collection - key` is
   unassigned. `08/delete-absent-key-outcome`.
   **RESOLVED (pending SPEC.md merge):** §8.9 amended (delete-by-key is a set
   operation); case pinned → `ok`/`unchanged`.
8. **Failure class for unresolvable/ungranted names.** §10.4/§12.1 mandate
   failure but pin no category (denied vs not-found), and don't require a
   nonexistent surface to be indistinguishable from an ungranted one
   (enumeration leak). `10/unresolvable-name`,
   `11/public-surface-authenticator-selection`.
9. **`$actor` with no actor bound.** Public request and host-operator
   transition: static reject (§6.2) vs admission reject (§6.3) vs null
   binding; whether host provenance is application-readable is also unpinned.
   `22/public-request-references-actor`, `23/operator-commit-actor-provenance`.
   **RESOLVED (pending SPEC.md merge):** §22.8 amended (fail-closed; host
   provenance not application-readable); both cases pinned → `rejected`.
10. **Surface `$view` parameter inference.** Inference is defined for
    mutations only (§8.3). `06/surface-view-parameter-inference`.
    **RESOLVED (pending SPEC.md merge):** §10.1 amended — a surface `$view`
    (and a `$recursive` `$where`/`$except` predicate) parameter is *not*
    inferred; every `@name` it reads MUST be declared in `$params`, an
    undeclared one is a static load error (§8.3 inference is mutation-only).
    The public `$view` path already rejected via full typing; the fix extends
    the undeclared-parameter rejection to the role `$view` path (which skips
    full typing for the `$actor` seam). Corpus `06/…-invalid` (public) and new
    `10/role-view-undeclared-parameter-invalid` (role) flip to `invalid`.
11. **Interface addressing edges.** Recursive-descendant mutation addressing;
    `$where`/`$except` excluded-branch representation; empty surface
    declaration. `10/recursive-descendant-mutation-addressing`,
    `10/where-excluded-branch`, `10/empty-surface-declaration`.
    **RESOLVED (pending SPEC.md merge):** §10.5/§10.1 amended, all three pinned
    single-canonical / fail-closed. (a) A covered descendant receiver is
    addressed by a descendant KEY PATH extending the role scope; admission
    re-walks the relation and denies any uncovered step (the role-holding row
    is the empty path). (b) `$where` is hereditary exactly like `$except`:
    recursion descends only into included candidates, and a `$where`-excluded
    or `$except`-pruned node's descendants are neither surfaced nor reparented
    (`$where` allow-list, `$except` deny-list, deny overrides). (c) A surface
    exposing neither `$view` nor `$mut` (empty, or `$params`/`$recursive`-only)
    is rejected at load. Runtime: (c) landed in `liasse-model` (empty-surface
    reject); (a) descendant addressing and (b) recursive materialization stay
    runtime debt (scoped-role addressing and recursive-coverage view are
    validated but not yet materialized), so the two scenario corpus cases are
    pinned to their spec outcomes and acknowledged in the scenario ledger.

## Views

12. **Synthetic `$key` constraint scope.** §7.2 states the
    "aggregated or key-derived" constraint unconditionally yet allows a
    synthetic key "for grouping OR a new identity"; application to a per-row
    unique identity is undisambiguated.
    `07/synthetic-key-new-identity-nonaggregated`.
    **RESOLVED (pending SPEC.md merge):** §7.2 amended (constraint is
    unconditional); case pinned → `invalid`.

## Buckets, meters, timing

13. **Enforcement points.** Negative projected pool `$quantity`: reject at
    insert vs at meter evaluation. Calendar `overflow: reject`: eager at the
    source transition vs lazy at the temporal read. Connector resolution
    timing. `15/negative-pool-quantity-enforcement`,
    `14/overflow-reject-detection-timing`, `18/connector-resolution-timing`.
14. **Funding-row projected metadata.** Whether a source-projected member
    (e.g. `price`) appears in each §15.3 funding row.
    `w-worked-examples/w3-funding-projected-metadata-shape`.
    **RESOLVED (pending SPEC.md merge):** §15.3/§15.6 amended (fixed
    `{source,pool,amount}` shape); W3 case pinned → `ok` with that shape.

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
    **RESOLVED (pending SPEC.md merge):** §18.8/§18.9 amended (runtime fetch
    recovers across verified holders); tampered case pinned → `ok` + true bytes,
    all-corrupt case pinned → `error` (no-clean-holder).
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
    **PARTIALLY RESOLVED (pending SPEC.md merge):** §9.1 amended for (A) sibling
    visibility and (B) one-shot `= expr` freeze; `05/seed-default-sibling-visibility`
    pinned → `(3,3)`. (B)'s `w4` case stays blocked (SKIP ledger, §13 child-compile
    seam). **ESCALATE (impl does not match):** leg (C) reload-genesis-only — the
    engine returns completion `committed`, not `unchanged`, on a byte-identical
    reload (consistent with §9.3's "a definition-only update creates a commit"),
    so the §9.4 clause and `09/reload-divergent-seed-data` pin were left out.
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
    **RESOLVED (pending SPEC.md merge):** all four pinned as fail-closed static
    load errors — §4.2/C.4 (bare `=` empty body), §5.3/C.2 (two mutually-
    exclusive kind markers, names both), §7.4/C.8 (`|`/`&` share one precedence
    level, mixed chains MUST be parenthesized with `( )`, else a static error;
    homogeneous chains stay left-associative), §8.1/C.9 (empty program array).
    Runtime: an explicit conflicting-marker check and a mixed-combinator
    rejection added; the empty-`=` seed and empty-program array already rejected.
    The four corpus cases are now `-invalid`.

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

## Gaps surfaced by implementation red-teaming

These were found while attacking the implementation for spec divergences;
each is a place the spec genuinely does not pin behavior (distinct from an
implementation bug, which gets fixed against the spec instead).

28. **`json` number scale is unbounded.** Annex A.6 bounds `decimal` scale
    (the implementation rejects extreme exponents at the wire boundary), but a
    number inside a `json` value has no such bound, so a `json` field carrying
    `1E-2000000000` forces an unbounded digit-string allocation during A.7
    canonicalization — a hang, not a clean rejection. The spec should bound
    `json` number scale the same way it bounds `decimal`.

29. **`optional<json>` cannot represent a `json` object equal to the none
    sentinel.** A.1 encodes a generic-slot `none` as `{"$none": true}`, and a
    legitimate `json` object whose literal shape is `{"$none": true}` encodes
    identically. Under `optional<json>`, decode resolves those bytes to `none`,
    so a representable `json` value is lost on round-trip. A.1/A.7 do not
    disambiguate a `json` object equal to the sentinel.

30. **Absent optional member ordering within a composite value.** B.2 pins
    present-before-`none` for a top-level `optional<T>` sort column, but does
    not state whether a descriptor's absent optional member (e.g. a blob
    `$name`, or a calendar period `zone`) follows the same none-last rule
    inside B.4's structural ordering. A one-line B.4 clarification would settle
    whether absent sorts before or after present.

31. **Empty-`text` key vs display-path round-trip.** A.1/A.8 make an empty
    string a legal `text` key value and D.2 pins its key text as empty, but an
    empty component makes a rendered display path non-injective / non-round-
    trippable (`/it//x` style). The spec pins neither a prohibition on empty
    key components nor an escaping that keeps display paths reversible.

32. **`text` values containing `U+0000`.** A.1 defines `text` as a sequence of
    Unicode scalar values and does not exclude `U+0000`, yet PostgreSQL — the
    mandated backend illustration — cannot store a NUL inside `jsonb`. The spec
    should either exclude `U+0000` from `text` or require an encoding that
    survives the storage layer, so conforming backends agree. (The
    implementation must still make its two backends agree regardless — tracked
    as a fix, not left here.)
    **RESOLVED (pending SPEC.md merge):** Annex A.1 amended (U+0000 legal;
    backend-preservation obligation) with a §23.7 cross-reference. No logical
    corpus case added: backend preservation is impl-tested (memory-vs-PG
    agreement in `liasse-pg`), and a literal NUL in a corpus file is fragile.

33. **§19.5 `entries` membership scope.** "`entries` covers every required
    direct archive entry other than `manifest.json`" states one exclusion, but
    entries with dedicated manifest members (`liasse.json`, state, history) and
    child-module artifacts under `modules/` are also "required direct entries";
    whether each must additionally appear in `entries` is under-pinned.

34. **Extra member in a returned host value.** §5.8 structural satisfaction
    ("a value ... with the required fields ... satisfies the shape") read as
    width subtyping would accept a returned struct carrying an extra member;
    the strict closed-struct reading rejects it. This is the return-value
    analogue of item 5 (unknown members in call arguments): reject vs ignore
    is unstated. Surfaced by host-conformance red-teaming.
    **RESOLVED (pending SPEC.md merge):** §5.8 amended (closed structural
    satisfaction) and §16.3 (a nonconforming host return, incl. an extra member,
    is rejected as a §2.1 nonconformance). No corpus case added: the sim host has
    no extra-member-return behavior to drive one without engine changes, and the
    impl already rejects (`liasse-host` `conform.rs` `UnexpectedField`).

35. **§20.1 "compatible value is copied" across an incompatible scalar type
    change.** A `major`-bump migration that retypes a field with no `$from`/
    `$as` must, per §20.1, copy the "compatible" value — but §20.1 never
    defines "compatible" for a scalar *type* change (`text`→`int`,
    `decimal`→`int`, widen/narrow). The implementation pins the observable
    §22.1 outcome (no ill-typed value in committed state) by treating a value
    as committable iff it decodes under the target type's Annex-A/§19 portable
    codec: representable → coerced, otherwise rejected like an unpopulated
    required field. Under that boundary `decimal 1.0 → int` *rejects* (the
    canonical `decimal` wire keeps its scale, so `Integer::parse` refuses
    `"1.0"`). Whether §20.1 intends a lossless numeric down-conversion
    (`decimal 1.0`→`int 1`) as an implicit compatible copy, or requires an
    explicit `$as: "int(.)"`, is unpinned. Surfaced by migration red-teaming.
    **RESOLVED (pending SPEC.md merge):** §20.1 amended (compatible copy =
    representation-preserving; `decimal`→`int` needs explicit `$as`). New case
    `20/major-retype-decimal-to-int-no-transform-rejected` pinned → `rejected`.

36. **§19.9 merge-conflict coordinate for a §8.2 root-singleton member.** A
    three-way merge conflict on a root-singleton field is reported with the
    internal reserved collection name (`$root`) and an empty key rather than a
    §D.3 application address (e.g. `.flag`). The merge *semantics* are correct;
    only the `ConflictCoordinate` rendering leaks the internal name. §19.9/§D.3
    do not pin the coordinate form for a singleton member. Surfaced by
    merge/rollback red-teaming.
