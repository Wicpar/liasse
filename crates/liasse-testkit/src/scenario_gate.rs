//! The scenario conformance **gate**: the shared debt ledger ([`SKIP`]) and the
//! per-case classification ([`classify`]) both the memory and PostgreSQL scenario
//! runners enforce.
//!
//! A scenario case may end only three admissible ways for the gate to pass:
//!
//! - a clean [`CaseVerdict::Pass`];
//! - [`CaseVerdict::UnspecifiedObservations`] — the case exercises behaviour
//!   SPEC.md does not pin (tracked in `SPEC-ISSUES.md`), recorded but not judged;
//! - a `Fail`/`Skipped` verdict whose `"<area>/<name>"` key is on [`SKIP`], the
//!   explicit debt ledger, each entry carrying a one-line reason grouped by the
//!   missing capability.
//!
//! Any other case — a non-pass verdict absent from the ledger — is a gate
//! failure. Symmetrically, a ledger entry that has started passing is stale and
//! must be removed, so the list can only shrink. Both runners share this one
//! module so the two backends are gated against the *identical* expectation and a
//! verdict divergence between them is a store-contract bug, not a harness skew.

use crate::report::CaseVerdict;

/// The `"<area>/<name>"` ledger key for a case.
#[must_use]
pub fn key(area: &str, name: &str) -> String {
    format!("{area}/{name}")
}

/// The recorded debt reason for `key`, if it is on the ledger.
#[must_use]
pub fn skip_reason(key: &str) -> Option<&'static str> {
    SKIP.iter().find(|(k, _)| *k == key).map(|(_, reason)| *reason)
}

/// How one case's verdict sits against the gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateClass {
    /// A clean pass — always admissible.
    Pass,
    /// Recorded-but-unjudged unspecified observations — admissible (a documented
    /// `SPEC-ISSUES.md` gap the case cannot pin).
    Unspecified,
    /// A `Fail`/`Skipped` verdict the ledger acknowledges, with its reason.
    AcknowledgedDebt(&'static str),
    /// A `Fail`/`Skipped` verdict absent from the ledger — a gate failure.
    Unacknowledged,
}

/// Classify `verdict` for the case at `key` against the [`SKIP`] ledger.
#[must_use]
pub fn classify(verdict: &CaseVerdict, key: &str) -> GateClass {
    match verdict {
        CaseVerdict::Pass => GateClass::Pass,
        CaseVerdict::UnspecifiedObservations { .. } => GateClass::Unspecified,
        CaseVerdict::Fail { .. } | CaseVerdict::Skipped { .. } => match skip_reason(key) {
            Some(reason) => GateClass::AcknowledgedDebt(reason),
            None => GateClass::Unacknowledged,
        },
    }
}

/// Scenario cases that do not yet cleanly pass, each acknowledged as explicit
/// debt with a one-line reason, grouped by the missing capability. A case is
/// here because it currently ends in `Fail` or `Skipped`; `Pass` and
/// `UnspecifiedObservations` are never listed. This is the honest debt ledger:
/// the gate ([`classify`]) fails on any non-pass case absent from it, and the
/// authoring test fails on any entry that has since started passing, so the
/// list can only shrink. Keyed `"<area>/<name>"`.
pub const SKIP: &[(&str, &str)] = &[
    // ========================================================================
    // STEPS THE SURFACE DRIVER DOES NOT DRIVE THIS PHASE
    // The scenario adapter routes connect/call/watch/read/advance_time end to end,
    // but these step kinds need host wiring or store access the surface layer does
    // not yet expose (adapter/mod.rs documents the reach). Each such step reports a
    // harness skip, so the case is skipped, not judged.
    // ========================================================================
    // --- `authenticate` step ---
    // The `authenticate` step drives through `SurfaceHost::authenticate`; the
    // residual case fails later — its `watch`/`call` name a multiplexed `context`
    // (§11.8) the adapter does not yet thread onto the surface watch/call.
    // --- §18 blob steps drive the composed blob host end to end ---
    // The blob-parameter upload (`blob_put`, staged into the blob host and admitted
    // under the step's role selection), fetch (`blob_get`, gated through the caller's
    // surface projection via `BlobHost::fetch_projected`), and connector fault
    // injection (`connector_set`) run against a real §18 `BlobHost` the driver owns
    // (adapter/blobs.rs), composed from the case's `hosts.connectors` + `$data`
    // stores; the parameterized role/surface `$view` a fetch resolves through is
    // served by reconstructing its `$params` (adapter/surface_params.rs). The
    // residuals below are genuine runtime/model/expression seams, not driving gaps:
    //
    // §18.5 placement descriptor members (`.file.$satisfied`/`.file.$stored`/
    // `.file.$surplus`) now type-check and evaluate in the expression layer, and the
    // driver records the placement facts into the engine before the return/view read
    // (adapter/blobs.rs `stage`+`Engine::record_blob_placement`, §18.5). The cases
    // that read a placement member in a mutation `return` therefore pass; the two
    // residuals below stay blocked on a distinct §12.2 view-shape seam, not the
    // record-placement seam. A `claim` residual keeps its binding note.
    ("18-blobs/same-content-different-metadata-distinct-descriptors", "a declared `$name` must bind into the mutation call, which the honest blob parameter drops"),
    ("18-blobs/descriptor-bytes-encoding-unspecified", "a verifying client-declared descriptor must bind into the mutation call, which the honest-only blob parameter does not expose"),
    // §18.3 pins eager connector resolution: a store-row write feeding a declared
    // placement is rejected at admission when its connector is unregistered. The
    // runtime `Engine` holds no blob-connector registry (connectors live in the
    // driver's composed `BlobHost`, not `Engine::call`'s admission), so the store-row
    // insert commits as ordinary data instead of rejecting — a flagged follow-on hole
    // (wiring the connector registry into runtime admission is a subsystem-crossing
    // change), acknowledged here against the now-pinned §18.3 outcome.
    ("18-blobs/connector-resolution-timing", "eager store-row connector validation (§18.3) needs the connector registry threaded into runtime admission, which the engine does not hold"),
    // --- `budget_set` step ---
    ("23-host-contract/budget-backpressure-or-reject-choice-unspecified", "`budget_set` step not driven this phase"),
    ("23-host-contract/budget-exhaustion-never-partial-transition", "`budget_set` step not driven this phase"),
    // (§4 `build_artifact`/`repack_artifact`/`load_artifact` now drive the real
    // `liasse-artifact` archive layer — adapter/artifacts.rs — so every
    // 04-package-structure case passes.)
    // --- `erase` step ---
    ("annex-d-identity/erase-removes-live-row-and-rechecksums-history", "`erase` step not driven this phase"),
    ("annex-d-identity/erasure-extract-replay-foreign-instance-rejected", "`erase` step not driven this phase"),
    // §21.2 erasure now scrubs the full delete-closure history and exports the whole
    // closure (runtime `exec_erase`); the erase call and both live-removal views pass.
    // The residual `scrub_scope_of_cascaded_row` marker step confirms the cascaded
    // row's HISTORY stub, which the step vocabulary cannot read (no retained-history-
    // payload read is exposed) — an observability seam, not a behavior gap.
    ("21-deletion-erasure/erase-cascade-scrub-scope", "`scrub_scope_of_cascaded_row` marker needs a retained-history-payload read the step vocabulary does not expose; the runtime performs the closure-wide scrub"),
    // --- §19 history op families (export/import/reconcile/apply_correction, the
    // tamper and extract ops) now drive end to end (adapter/ops.rs, artifacts.rs,
    // correction.rs). The residual entries are genuine runtime/artifact seams, not
    // adapter driving gaps. ---
    // §19.7 state-section-vs-selected-point coherence is not verified at restore, so
    // a spliced or mismatched selection is accepted rather than rejected.
    ("19-history-artifacts/manifest-index-selection-mismatch-invalid", "§19.7 state/index selection coherence is not verified at restore, so a mismatched selection is accepted"),
    ("19-history-artifacts/spliced-state-selection-mismatch-invalid", "§19.7 state-vs-selected-point coherence is not verified at restore (the copy_entry_from splice is accepted)"),
    // §19.9 the runtime merge does not re-validate the combined composition under
    // ordinary rules, so an individually-clean union that breaks uniqueness is
    // reported clean instead of conflicting.
    ("19-history-artifacts/merge-combined-uniqueness-violation-conflict", "§19.9 the runtime merge does not re-validate the combined composition under uniqueness, so the invalid union is reported clean (applied true)"),
    // Tamper ops needing machinery beyond archive byte/JSON surgery.
    ("19-history-artifacts/forged-state-consistent-checksums-unspecified", "the `edit_cbor` tamper needs schema-owned resolution of a keyed-collection logical pointer into the state section, beyond byte surgery"),
    ("19-history-artifacts/history-index-overlapping-ranges-invalid", "the runtime emits an empty history-index `ranges` object (CORE), so `duplicate_json_member` has nothing to duplicate and §19.6 range verification is unlanded"),
    // §13 non-absolute module space / child-artifact embedding (same seam as the
    // child-* cases below).
    ("19-history-artifacts/merge-competing-module-mounts-conflict", "§13 the case mounts a non-absolute module space `mods` the runtime `ModuleSpace` rejects, so mount competition never reaches the merge"),
    // --- §20 `host_load` / migration ---
    // `host_load` drives `Engine::update` end to end (adapter/runtime.rs
    // `apply_host_load`). The §20.1 package-level `$migrations` program, the §20.2
    // reversible `$as`/`$back` transforms, and the `$from` nonexistent-source check
    // now land in the runtime, so those cases pass (their ledger entries were
    // pruned as stale). The single residual below is a corpus-vocabulary
    // discrepancy, not an implementation gap.
    //
    // CORPUS SUSPECT (§20.3 / Annex E.9 outcome vocabulary): `Engine::update`
    // returns `UpdateError::Rejected` (RejectionReason::Compatibility) for a boundary
    // narrowing, rendered `rejected`; the case expects `invalid` under the chapter's
    // definition-only §9.4 split. Both collapse to the one §9.4 `rejected` lifecycle
    // result, so the observable outcome is not in doubt — a corpus-classification
    // discrepancy, not an implementation bug (reported, not faked).
    ("20-evolution-migrations/minor-update-narrowing-despite-migration-rejected", "CORPUS SUSPECT: runtime emits `rejected` for the boundary narrowing (Annex E.9, admission-class), the case expects `invalid` (definition-only §9.4 split); both collapse to §9.4 `rejected` — a corpus-vocabulary discrepancy, not an impl bug"),
    // --- `keyring_admin` manual `bind_activate` (§17.4 manual policy) ---
    // `keyring_admin` now drives the engine's self-provisioned ring through
    // `Engine::keyring_admin` (adapter/keyrings.rs) — `revoke`/`destroy` on an
    // automatic (`$rotate`) ring pass. The manual `bind_activate` cases below stay
    // blocked on two liasse-runtime/liasse-host seams, not a testkit gap: (1) the
    // runtime bootstraps a no-`$rotate` keyring as *automatic* (it auto-activates
    // v1), whereas §17.1 (and these cases) read a missing `$rotate` as *manual*
    // (no active version until an operator binds one); (2) the engine's
    // self-provisioned `SimKeyProvider` offers a bindable external handle
    // (`MANUAL_EXTERNAL_KEY`) only for a manual-mode ring, and never the corpus's
    // named `external_keys` with their declared algorithms — so a manual
    // `bind_activate` has nothing to bind (a no-`$rotate` ring rejects
    // `UnknownExternal`), and `bind-algorithm-mismatch` cannot present a
    // wrong-algorithm external key. Provisioning them needs a `&mut`
    // add-external-key on `SimKeyProvider` or a no-`$rotate`=manual policy fix.
    ("17-keyrings/bind-algorithm-mismatch-rejected", "no-`$rotate` ring bootstraps as automatic and its provider carries no bindable external key with the corpus algorithm — a runtime policy / liasse-host provider-provisioning seam"),
    ("17-keyrings/manual-activation-enables-dependent-surface", "no-`$rotate` ring bootstraps as automatic (auto-activates v1) and its provider offers no bindable external handle, so manual `bind_activate` rejects — a runtime no-`$rotate`=manual / provider-provisioning seam"),
    ("17-keyrings/manual-second-activation-retires-prior", "no-`$rotate` ring bootstraps as automatic and its provider offers no bindable external handle, so manual `bind_activate` rejects — a runtime no-`$rotate`=manual / provider-provisioning seam"),
    // --- §13 module lifecycle (driven via `ModuleDeployment`, adapter/modules.rs) ---
    // The lifecycle outcomes (name validation, duplicate detection) route end to
    // end; the entries below are blocked on a runtime/surface seam the current
    // `ModuleDeployment` does not close, named per case.
    //
    // Parent-surface interface addressing: a root read/call addressing `.modules`
    // now routes through the deployment (adapter/modules.rs `root_view`/
    // `interface_call`), so the §13.9 aggregation and §13.10 interface mutation
    // reach the installed children. The entries below stay blocked on a distinct
    // seam named per case.
    ("13-modules/cross-module-atomic-transition", "§13.10 the root package's interface-addressed `$public` surfaces do not compile in a fresh engine, so the deployment cannot be built; cross-module atomic mutation dispatch is unlanded"),
    ("13-modules/parent-exposed-surface-row-local", "§13.4 `$parent` capability projection and interface-addressed `$public` surfaces are unlanded, so the root package does not compile in a fresh engine"),
    ("13-modules/rename-instance-stale-name-not-addressable", "the root's interface-addressed `$public` surfaces (`.modules[k]::iface`, `.modules::` aggregation) do not compile in a fresh engine, so the deployment cannot be built"),
    ("13-modules/uninstall-blocked-by-cross-boundary-restrict-ref", "§13.12 cross-boundary `$on_delete: restrict` blocking plus interface-addressed surfaces are unlanded, so the root package does not compile in a fresh engine"),
    ("13-modules/uninstall-unbinds-optional-peer", "§13.5/§13.12 optional-peer unbinding plus interface-addressed surfaces are unlanded, so the root package does not compile in a fresh engine"),
    // Interface-contract satisfaction: the child's exposed view/mutation vs the
    // parent's declared `$interfaces` is not checked at install.
    ("13-modules/expose-binding-contract-mismatch-invalid", "§13.8/§13.10 interface-contract satisfaction (the child exposed `$mut`/`$view` binding vs the parent's declared interface) is not checked at install, so the mismatch is admitted"),
    ("13-modules/interface-mutation-param-contract-mismatch-invalid", "§13.10 interface mutation parameter-contract satisfaction is not checked at install"),
    // Installation `$data` overlay (§13.3): the overlay is recorded on the install
    // request (adapter/modules.rs), so a fresh-row overlay and its `$check` now run;
    // the three-way *merge* onto an already-seeded row is a runtime seam.
    ("13-modules/install-data-overlay-merge", "§13.3 the installation `$data` overlay's three-way merge onto an existing package-`$data`-seeded row is not landed in the runtime, so the overlay over a seeded row is rejected rather than merged"),
    // Module-space existence (§13.2): the containing-row check is a documented seam.
    ("13-modules/install-into-nonexistent-space-rejected", "§13.2 the containing-row existence check for a module space is unlanded, so an install into a ghost row is admitted"),
    // Peer/`$deps` resolution (§13.5/§13.6): the consumer child fails a standalone
    // compile (an unresolved `#peer` handle, or no `$model`) before peer/dep
    // binding can run, so a peer-admission `rejected` is observed as a static
    // `invalid` instead.
    ("13-modules/peer-zero-candidates-rejected", "§13.5 peer resolution is unlanded; the consumer child (no `$model`, unresolved `#peer`) fails standalone compile, observed `invalid` rather than the peer-admission `rejected`"),
    ("13-modules/peer-single-candidate-auto-binds", "§13.5 peer auto-binding and `#handle` read-through are unlanded"),
    ("13-modules/peer-multiple-candidates-need-explicit-binding", "§13.5 peer resolution against the sibling set is unlanded, so the root/child does not build a resolvable deployment"),
    ("13-modules/peer-incompatible-major-rejected", "§13.5 peer major-compatibility resolution is unlanded; the consumer child fails standalone compile before a candidate can be rejected"),
    ("13-modules/peer-lookup-stays-in-sibling-space-rejected", "§13.5 peer resolution scoping to the sibling space is unlanded; the consumer child fails standalone compile"),
    ("13-modules/required-peer-disabled-candidate-rejected", "§13.5/§13.12 peer resolution that skips disabled candidates is unlanded; the consumer child fails standalone compile"),
    ("13-modules/explicit-peer-binding-cross-space-rejected", "§13.5 peer/`$use` explicit-binding resolution is unlanded; the consumer child fails standalone compile before a cross-space binding can be rejected"),
    ("13-modules/optional-peer-absent-valid", "§13.5 optional-peer `#handle` read-through is unlanded; the child fails standalone compile / the root interface-addressed surface does not compile"),
    ("13-modules/private-deps-isolated-per-consumer", "§13.6 `$deps` private nested-instance provisioning is unlanded; the consumer child fails standalone compile"),
    ("13-modules/sibling-cannot-address-private-dep-rejected", "§13.6 `$deps` privacy/provisioning is unlanded; the consumer child fails standalone compile"),
    // `$if_module` guard (§13.7).
    ("13-modules/if-module-guarded-state-preserved", "§13.7 `$if_module`-guarded `$expose` declarations are rejected by the model grammar (unlanded), so the child fails to load"),
    // Update path (§13.14/§13.15): the §20 migration does not enforce the narrowing
    // recheck, and the runtime host does not assemble the §13.15 update report.
    ("13-modules/minor-update-narrowing-rejected", "§13.14 the narrowing recheck (exposed-compatibility-surface preservation) is not enforced by the §20 update the runtime host runs"),
    ("13-modules/update-narrowing-view-field-rejected", "§13.14 the narrowing recheck (dropping an exposed view field) is not enforced by the §20 update the runtime host runs"),
    ("13-modules/update-result-report", "§13.15 the update-report shape ($instance/$from/$to/$migrated/$seeded/$exposed/$imports/$commit) is not assembled by the runtime host, which returns a §20 migration report"),
    ("13-modules/update-seed-three-way-merge", "the child `set_label` mutation defeats §8.3 parameter inference in a standalone child compile (M-MUT), so install fails before the §13.13 seed merge and `.modules::` aggregation can run"),
    // --- §13 module lifecycle used by other chapters (same seams) ---
    ("19-history-artifacts/child-export-matches-embedded-artifact", "§19 child-module `.liasse` artifact export/embedding is unlanded; the case also mounts a non-absolute module space `mods` the runtime `ModuleSpace` rejects"),
    ("19-history-artifacts/child-module-artifact-embedded-and-extractable", "§19 child-module `.liasse` artifact export/embedding is unlanded; the case also mounts a non-absolute module space `mods` the runtime `ModuleSpace` rejects"),
    ("19-history-artifacts/tampered-child-artifact-invalid", "§19 child-module `.liasse` artifact embedding/verification is unlanded; the case also mounts a non-absolute module space `mods` the runtime `ModuleSpace` rejects"),
    ("annex-e-compatibility/module-minor-rebinds-interface-implementation-accepted", "§13.15/Annex E the update-report shape is not assembled by the runtime host, so the asserted report value is absent"),
    ("annex-e-compatibility/module-removes-interface-binding-rejected", "§13.14/Annex E the interface-binding-removal narrowing recheck is not enforced by the §20 update the runtime host runs"),
    ("w-worked-examples/w4-confusable-instance-names-both-install-distinct", "the child module package fails a standalone compile in the current runtime, so install is observed `invalid` rather than `ok` (a §13 child-compile seam)"),
    ("w-worked-examples/w4-host-imports-exposed-template-across-boundary", "§13.9 the root package's interface-addressed `$public` surfaces do not compile in a fresh engine, so the deployment cannot be built"),
    ("w-worked-examples/w4-import-disabled-template-across-boundary-rejected", "§13.9/§13.12 the root package's interface-addressed surfaces do not compile in a fresh engine, so the deployment cannot be built"),
    ("w-worked-examples/w4-install-exposes-enabled-template-to-parent", "§13.8/§13.9 the root package's interface-addressed surfaces do not compile in a fresh engine, so the deployment cannot be built"),
    ("w-worked-examples/w4-plan-gates-template-disabled-not-exposed", "the child module package fails a standalone compile in the current runtime, so install is observed `invalid` rather than `ok` (a §13 child-compile seam)"),
    ("w-worked-examples/w4-seed-computed-enabled-reevaluation-unspecified", "the root package's interface-addressed surfaces do not compile in a fresh engine, so the deployment cannot be built (SPEC-ISSUES #23 seed-reevaluation case)"),
    ("w-worked-examples/w4-uninstall-removes-aggregated-templates", "the child module package fails a standalone compile / §13.9 aggregation is unlanded, so install is observed `invalid` rather than `ok`"),
    // --- `operator` step ---
    // Root-mutation operator transitions now drive through a synthetic public
    // surface (`SurfaceHost::operator_call`); the entries below remain debt for a
    // distinct reason the operator wiring does not resolve.
    ("23-host-contract/operator-retains-meter-capacity", "`operator` on a collection-row mutation needs receiver-row wiring"),
    // --- `restart` step ---
    // ========================================================================
    // HOST-ENVIRONMENT SHAPING INVARIANTS (ENGINE)
    // The step reached the engine but tripped an engine invariant while shaping the
    // evaluation environment (a row/collection the runtime could not present in the
    // expected shape). A runtime gap, surfaced as a host fault and skipped.
    // ========================================================================
    // --- hostfault:nested ---
    ("05-state-model/like-recursion-adopts-containing-shape", "host environment nested-collection shaping gap (engine invariant)"),
    // --- hostfault:row-field ---
    ("05-state-model/named-type-recursive-shape", "host environment row-field shaping gap (engine invariant)"),
    ("06-expressions/caret-reads-lexical-parent-scope", "host environment row-field shaping gap (engine invariant)"),
    ("annex-c-grammar/computed-field-equals-prefix-form", "host environment row-field shaping gap (engine invariant)"),
    ("annex-c-grammar/deep-nested-projection-loads", "host environment row-field shaping gap (engine invariant)"),
    ("annex-c-grammar/member-order-carries-no-semantics", "host environment row-field shaping gap (engine invariant)"),
    ("annex-c-grammar/plain-object-is-static-struct", "host environment row-field shaping gap (engine invariant)"),
    // ========================================================================
    // PACKAGE DOES NOT LOAD YET (UPSTREAM COMPILE/MODEL GAP)
    // The definition fails static validation or seed admission, so the case never
    // reaches its scenario steps. The corpus expects it to load and run; closing the
    // gap lives in the model/compile/seed layers below the runner.
    // ========================================================================
    // --- load ---
    ("10-interfaces-roles/duplicate-membership-no-extra-authority", "package does not load yet (upstream compile/model gap)"),
    // 11-auth session/host-verifier wiring is live (adapter/auth.rs); these
    // residual cases need a seam the auth wiring does not reach: a role-scoped
    // `$actor` view rendering, a scoped-role session mutation, or a session
    // collection with no `expires`/bucket lower bound.
    ("11-auth-sessions/committed-request-final-after-revocation", "scoped-role session `revoke()` mutation not bound (denied)"),
    ("11-auth-sessions/session-not-yet-active-denied", "session bucket lower-bound activation not observed at boundary"),
    // §14.5 bounded temporal read of an unbounded recurring source-backed bucket
    // now generates the series to the selector's own bound, so `.$at`/`.$between`
    // past the clock resolve; the rollover-at-boundary, future-spanning window, and
    // calendar-monthly-clamp cases pass and were pruned from the ledger.
    ("14-buckets/dst-fall-back-ambiguous-earlier", "package does not load yet (upstream compile/model gap)"),
    // §16 registered host namespaces now resolve strictly (`Engine::load_with_hosts`,
    // adapter/namespaces.rs), so these packages load and a host call in a collection
    // view/default runs (`generated-default-fixed-and-recorded` passes). These three
    // read a host call through a *root-scalar* view (`stats: ".doubled"` over
    // `doubled: "util.double(.n)"`), and the runtime materializes no row for a
    // top-level scalar view — a plain `.n` root-scalar view is empty too — so the
    // step-0 watch diverges before `reopen` is ever reached. A runtime root-scalar
    // view seam, not a §16 wiring gap.
    ("16-host-namespaces/pinned-descriptor-drift-fails-reopen", "root-scalar host-call view yields no row (runtime materializes no top-level scalar view; a plain `.n` view is empty too), so the step-0 watch diverges before `reopen`"),
    ("16-host-namespaces/required-namespace-removed-fails-reopen", "root-scalar host-call view yields no row (runtime materializes no top-level scalar view), so the step-0 watch diverges before `reopen`"),
    // §18 blob views: a parameterized surface/top-level `$view` a case reads now
    // compiles and serves (adapter/surface_params.rs reconstructs its `$params`),
    // and the §18.5 placement facts are recorded into the engine before a placement
    // member is read (adapter/blobs.rs + runtime.rs, §18.5). The residuals below are
    // an aggregate-over-projected-member type-check gap, a background reconciler
    // step, or the §12.2 keyed-selection surface-view result shape — none of them
    // the record-placement seam.
    ("18-blobs/billing-sum-over-stored-descriptors", "§18.11 the billing view `sum(.uploads[:u | /stores['primary'] in u.file.$stored].file.$bytes)` does not type-check — the aggregate-over-projected-member seam (`in`/`sum` over the projected `.file.$stored`/`.file.$bytes` placement member), so the package does not load"),
    ("18-blobs/corrupt-copy-demoted-and-repaired", "the placement view now resolves and steps 0–2 pass; the residual is the `run_reconciler` step (a background reconciler loop over retained lineages that demotes and repairs a corrupt copy), which the single-step `reconcile`/`apply_correction` verbs do not model — the run_reconciler seam"),
    ("18-blobs/descriptor-metadata-readable-in-view", "§12.2 keyed-selection surface view (`.docs[@id] { … }`) delivers a row array, but the case expects a single object — a runtime/corpus view-shape tension, not a driving gap"),
    ("18-blobs/metadata-only-projection-grants-no-fetch", "§12.2 keyed-selection surface view delivers a row array, but the case's metadata watch expects a single object — a runtime/corpus view-shape tension reached before the `blob_get` gate"),
    ("18-blobs/placement-observations-single-store", "the §18.5 placement facts are recorded and the placement members resolve, but the keyed-selection surface view (`.docs[@id] { … }`) delivers a §12.2 row array while the case expects a single object — the same view-shape tension as `descriptor-metadata-readable-in-view` (and in conflict with `06-expressions/selector-scalar-key-zero-or-one-row`, which expects the array form); a view-shape seam, not the record-placement seam"),
    ("18-blobs/surplus-copy-after-policy-shrinks", "the §18.5 placement facts are recorded and re-recorded on the store `enabled` shrink (adapter refresh), but the keyed-selection placement view (`.docs[@id] { … }`) delivers a §12.2 row array while the case expects a single object — the same view-shape seam as `placement-observations-single-store`, not the record-placement seam"),
    ("23-host-contract/rotation-provider-invalid-public-key-keeps-current-active", "package does not load yet (upstream compile/model gap)"),
    ("annex-d-identity/ref-wire-value-is-current-typed-key", "package does not load yet (upstream compile/model gap)"),
    ("w-worked-examples/w2-cross-account-session-revoke-has-no-owner-check", "package does not load yet (upstream compile/model gap)"),
    ("w-worked-examples/w2-disabled-account-fails-actor-check", "package does not load yet (upstream compile/model gap)"),
    ("w-worked-examples/w2-expired-session-token-replay-denied", "package does not load yet (upstream compile/model gap)"),
    ("w-worked-examples/w2-login-claims-unlinked-account-rejected", "package does not load yet (upstream compile/model gap)"),
    ("w-worked-examples/w2-login-subject-confusable-no-match-rejected", "package does not load yet (upstream compile/model gap)"),
    ("w-worked-examples/w2-one-connection-multiplexes-two-account-sessions", "package does not load yet (upstream compile/model gap)"),
    ("w-worked-examples/w2-passkey-login-opens-session-and-authenticates", "package does not load yet (upstream compile/model gap)"),
    ("w-worked-examples/w2-revoked-session-denies-future-requests", "package does not load yet (upstream compile/model gap)"),
    ("w-worked-examples/w2-two-logins-create-distinct-sessions", "package does not load yet (upstream compile/model gap)"),
    // --- seed ---
    ("15-meters/spend-time-in-gap-between-periods-unfunded", "seed admission gap: seed field `period`: `duration` value `= none` is not a canonical ISO-8601 elapsed dura..."),
    // ========================================================================
    // RUNTIME RESULT DIVERGES FROM THE CORPUS EXPECTATION
    // The package loads and the steps run, but the runtime's observed outcome, value,
    // or view result does not yet match what the case (re-derived from SPEC.md)
    // expects. Each is real conformance debt for the triage loop; none is edited away.
    // ========================================================================
    // --- fail:noval ---
    ("15-meters/hypothetical-balance-accessor-with-time", "no value produced (unsupported call path)"),
    ("21-deletion-erasure/double-reinsert-second-finds-no-stub-rejects", "no value produced (unsupported call path)"),
    ("21-deletion-erasure/reinsert-tampered-extract-hash-rejects", "no value produced (unsupported call path)"),
    ("annex-c-grammar/mutation-name-explicit-prototype-parses", "no value produced (unsupported call path)"),
    ("annex-c-grammar/noparam-call-paren-equals-empty-args", "no value produced (unsupported call path)"),
    ("annex-c-grammar/set-field-form-add-and-remove-members", "no value produced (unsupported call path)"),
    // --- fail:noview ---
    ("10-interfaces-roles/except-prunes-entire-branch", "no view value produced (unsupported view/watch path)"),
    ("10-interfaces-roles/recursive-coverage-nests-included-descendants", "no view value produced (unsupported view/watch path)"),
    // §10.5 hereditary-$where pin (SPEC-ISSUES #11(b)): recursive coverage is
    // validated statically but not materialized at runtime, so the covered view
    // produces no value — same seam as the two recursive cases above.
    ("10-interfaces-roles/where-excluded-branch-hereditary", "no view value produced (unsupported view/watch path); §10.5 recursive coverage validated but not materialized at runtime"),
    ("14-buckets/short-form-from-defaults-to-created", "no view value produced (unsupported view/watch path)"),
    // --- fail:outcome ---
    ("05-state-model/nested-initializer-failure-rejects-parent-insert", "outcome divergence: expected `ok` observed `rejected`"),
    ("06-expressions/row-mutation-receiver-duplicate-occurrences-reject", "outcome divergence: expected `ok` observed `rejected`"),
    ("10-interfaces-roles/deleted-scope-row-revokes-role", "outcome divergence: expected `ok` observed `denied`"),
    ("10-interfaces-roles/fixed-call-argument-not-overridable", "outcome divergence: expected `ok` observed `denied`"),
    ("10-interfaces-roles/membership-reevaluated-each-admission", "outcome divergence: expected `ok` observed `rejected`"),
    // §10.5 descendant key-path addressing pin (SPEC-ISSUES #11(a)): the case's
    // step-2 scoped-role rename already observes `denied` (scoped-role addressing
    // is unwired this phase, same seam as `scoped-role-addressed-by-row-and-name`);
    // step-3 descendant re-walk addressing is likewise unlanded.
    ("10-interfaces-roles/recursive-descendant-mutation-addressing", "outcome divergence: expected `ok` observed `denied` — scoped-role (and §10.5 descendant key-path) addressing unwired this phase"),
    ("10-interfaces-roles/row-mutation-receiver-exactly-one", "outcome divergence: expected `rejected` observed `denied`"),
    ("10-interfaces-roles/scoped-role-addressed-by-row-and-name", "outcome divergence: expected `ok` observed `denied`"),
    ("11-auth-sessions/login-operation-id-replay-at-most-once", "outcome divergence: expected `ok` observed `rejected`"),
    ("11-auth-sessions/login-operation-id-reuse-different-request-rejected", "outcome divergence: expected `ok` observed `rejected`"),
    ("11-auth-sessions/login-token-immediately-usable", "outcome divergence: expected `ok` observed `rejected`"),
    ("12-clients-live-views/parameter-normalization-and-checks", "outcome divergence: expected `ok` observed `denied`"),
    ("15-meters/hierarchical-level-without-meter-adds-no-constraint", "outcome divergence: expected `ok` observed `rejected`"),
    ("15-meters/hierarchical-limits-clear-every-level", "outcome divergence: expected `ok` observed `rejected`"),
    ("15-meters/inactive-bucketed-spend-retains-allocation", "outcome divergence: expected `ok` observed `rejected`"),
    ("15-meters/plan-downgrade-preserves-recorded-funding", "outcome divergence: expected `ok` observed `rejected`"),
    ("15-meters/pool-removal-preserves-recorded-funding", "outcome divergence: expected `ok` observed `rejected`"),
    ("15-meters/spend-at-pool-until-boundary-excluded", "outcome divergence: expected `ok` observed `rejected`"),
    ("15-meters/spend-update-failure-preserves-prior-allocation", "outcome divergence: expected `ok` observed `rejected`"),
    ("23-host-contract/restart-preserves-identity-values-and-view", "outcome divergence: expected `ok` observed `denied`"),
    // --- fail:valdiff ---
    // --- fail:viewdiff ---
    ("05-state-model/set-of-enum-reads-in-declaration-order", "view result diverges from expectation"),
    ("12-clients-live-views/temporal-observation-advances-live-view", "view result diverges from expectation"),
    ("12-clients-live-views/window-anchor-survives-rekey", "view result diverges from expectation"),
    ("14-buckets/expiration-preserves-row-in-all", "view result diverges from expectation"),
    ("22-runtime-semantics/concurrent-appends-either-order-both-atomic", "view result diverges from expectation"),
    ("22-runtime-semantics/cross-connection-sequential-order-unspecified", "view result diverges from expectation"),
    ("annex-c-grammar/patch-block-is-one-statement", "view result diverges from expectation"),
    // ========================================================================
    // CONCURRENT RUNTIME REGRESSION (not a testkit capability gap)
    // This case passed at HEAD and is unaffected by any adapter change (its only
    // step is a step-0 `call`). It regressed under the concurrent runtime rework
    // of the mutation/admission pipeline: a zero-capacity `$consumes` spend is no
    // longer rejected (§15.1/§15.2). The corpus expectation is spec-correct and
    // must not change; the fix belongs in the runtime. This entry auto-flags
    // stale once meter admission re-enforces zero capacity — prune it then.
    // ========================================================================
];
