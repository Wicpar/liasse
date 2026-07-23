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
    ("18-blobs/descriptor-bytes-encoding", "a verifying client-declared descriptor must bind into the mutation call, which the honest-only blob parameter does not expose; the harness's DeclaredDescriptor also erases the $bytes string-vs-number wire spelling (#20)"),
    // §18.3 pins eager connector resolution: a store-row write feeding a declared
    // placement is rejected at admission when its connector is unregistered. The
    // runtime `Engine` holds no blob-connector registry (connectors live in the
    // driver's composed `BlobHost`, not `Engine::call`'s admission), so the store-row
    // insert commits as ordinary data instead of rejecting — a flagged follow-on hole
    // (wiring the connector registry into runtime admission is a subsystem-crossing
    // change), acknowledged here against the now-pinned §18.3 outcome.
    ("18-blobs/connector-resolution-timing", "eager store-row connector validation (§18.3) needs the connector registry threaded into runtime admission, which the engine does not hold"),
    // --- `budget_set` step ---
    ("23-host-contract/budget-exhaustion-rejects-not-backpressure", "`budget_set` step not driven this phase"),
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
    ("19-history-artifacts/manifest-included-range-stated-in-coverage", "§19.5 the `coverage` member (included range + restorability) is not yet emitted by the exporter or carried by the manifest struct"),
    ("19-history-artifacts/manifest-index-selection-mismatch-invalid", "§19.7 state/index selection coherence is not verified at restore, so a mismatched selection is accepted"),
    ("19-history-artifacts/spliced-state-selection-mismatch-invalid", "§19.7 state-vs-selected-point coherence is not verified at restore (the copy_entry_from splice is accepted)"),
    // §19.9 the runtime merge does not re-validate the combined composition under
    // ordinary rules, so an individually-clean union that breaks uniqueness is
    // reported clean instead of conflicting.
    ("19-history-artifacts/merge-combined-uniqueness-violation-conflict", "§19.9 the runtime merge does not re-validate the combined composition under uniqueness, so the invalid union is reported clean (applied true)"),
    // Tamper ops needing machinery beyond archive byte/JSON surgery.
    ("19-history-artifacts/forged-state-consistent-checksums-accepted", "the `edit_cbor` tamper needs schema-owned resolution of a keyed-collection logical pointer into the state section, beyond byte surgery"),
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
    // `keyring_admin` drives the engine's self-provisioned ring through
    // `Engine::keyring_admin` (adapter/keyrings.rs). The runtime now reads a
    // missing `$rotate` as *manual* (§17.1: no active version until an operator
    // binds one), and the self-provisioned `SimKeyProvider` carries the bindable
    // `MANUAL_EXTERNAL_KEY` handle for every manual ring — so
    // `manual-activation-enables-dependent-surface` and
    // `manual-second-activation-retires-prior` now pass end to end.
    //
    // The one residual below stays blocked on a distinct liasse-host provider-
    // provisioning seam, NOT the policy: `bind-algorithm-mismatch-rejected` binds
    // a WRONG-algorithm external key (`bad-ed25519` into an `ES256` ring) and
    // expects the §17.4/§17.6 metadata validation to reject it. The adapter binds
    // the single `MANUAL_EXTERNAL_KEY` the double provisions in the ring's declared
    // algorithm, so it cannot present the corpus's named `external_keys` with their
    // own (mismatched) algorithms. Closing it needs a `&mut` add-external-key on
    // `SimKeyProvider` plus an adapter that binds the step's named external.
    ("17-keyrings/bind-algorithm-mismatch-rejected", "adapter binds the double's single declared-algorithm `MANUAL_EXTERNAL_KEY`, so it cannot present the corpus's wrong-algorithm named external key — a liasse-host provider-provisioning seam (add-external-key), not the no-`$rotate`=manual policy (now fixed)"),
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
    ("13-modules/update-bundle-three-way-merge", "§4.1 `$bundle` is not accepted by the model layer yet (explicit unimplemented rejection) and the §13.13 bundle three-way merge is unwired, so install fails before the merge can run"),
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
    // HOST-ENVIRONMENT SHAPING INVARIANTS (ENGINE) — CLOSED
    // Both env-shaping seams are now closed. The §5.8 self-referential
    // nested-collection shaping gap resolves a `$types`/`$like` keyed member to its
    // collection everywhere the runtime walks the state tree. The §5.2/§5.3 static
    // struct shaping gap folds each static struct's read-only computed values onto
    // its materialized struct-row (with `^` resolving to the containing row, §6.2)
    // and carries a keyless nested-struct projection inline as a `Value::Struct`, so
    // a struct-nested computed value, a `^` lexical-parent read, a computed field,
    // and a deep keyless projection all materialize — those cases pass and their
    // entries were removed here.
    // ========================================================================
    // PACKAGE DOES NOT LOAD YET (UPSTREAM COMPILE/MODEL GAP)
    // The definition fails static validation or seed admission, so the case never
    // reaches its scenario steps. The corpus expects it to load and run; closing the
    // gap lives in the model/compile/seed layers below the runner.
    // ========================================================================
    // --- load ---
    ("10-interfaces-roles/duplicate-membership-no-extra-authority", "package does not load yet (upstream compile/model gap)"),
    // 11-auth session/host-verifier wiring is live (adapter/auth.rs); this
    // residual case needs a seam the auth wiring does not reach: a scoped-role
    // inline `$mut` (`/sessions[$session.$key].revoke()`) reading the request-scoped
    // `$session`, which the surface router does not bind. (The bucket-expiry
    // reconstruction is now derived from the collection's `$bucket` `$until`, so a
    // session with an explicit `$from` lower bound activates at its boundary —
    // `session-not-yet-active-denied` passes and was pruned here.)
    ("11-auth-sessions/committed-request-final-after-revocation", "scoped-role session `revoke()` mutation not bound (denied)"),
    // §14.5 bounded temporal read of an unbounded recurring source-backed bucket
    // now generates the series to the selector's own bound, so `.$at`/`.$between`
    // past the clock resolve; the rollover-at-boundary, future-spanning window, and
    // calendar-monthly-clamp cases pass and were pruned from the ledger.
    ("14-buckets/dst-fall-back-ambiguous-earlier", "package does not load yet (upstream compile/model gap)"),
    // §16 registered host namespaces resolve strictly (`Engine::load_with_hosts`,
    // adapter/namespaces.rs). Under the §16.5 mutation-only rule (Phase 7b) a host
    // call in a database-evaluated position is a load error, so these cases were
    // recast to run `util.double` inside a mutation body (`add`) that a first step
    // exercises live. The residual is the `reopen` re-validation itself: the
    // memory adapter's reopen does not re-resolve the recorded descriptor against
    // the replaced host context (a drifted interface hash / a removed namespace),
    // so the `reopen` step does not yet produce the §9.2 open-time diagnostic. A
    // reopen re-validation seam, not a §16.5 wiring gap (the recast package loads,
    // the descriptor runs live before the swap).
    ("16-host-namespaces/pinned-descriptor-drift-fails-reopen", "reopen does not re-validate the recorded pinned descriptor against a drifted interface hash on the replaced host context (§9.2 open-time validation seam); the recast package loads and runs util.double live before the swap"),
    ("16-host-namespaces/required-namespace-removed-fails-reopen", "reopen does not re-validate a removed required namespace against the replaced (empty) host context (§9.2 open-time validation seam); the recast package loads and runs util.double live before the swap"),
    // Phase 7b §16.5 recast: an app verifier can no longer sit in `$verify`; the
    // case re-models onto a §11.5 login mutation invoking `authns.check` in its
    // body. Fully DRIVING it needs the testkit to carry a registered verifier's
    // declared `accepts` table onto the registered-namespace dispatch path used
    // inside a mutation body (adapter/namespaces.rs `read_function` wires `accepts`
    // only into the auth-layer `$verify` verifier, not the mutation dispatch), a
    // testkit enablement outside Phase 7b's scope. The recast package is
    // spec-correct and loads; the login step is acknowledged debt until that lands.
    ("16-host-namespaces/verifier-namespace-runs-at-admission", "recast onto the §11.5 auth-mutation pattern; the app verifier `authns.check` invoked in the mutation body needs the testkit to carry the verifier `accepts` table onto the registered-namespace dispatch path (currently wired only into the auth-layer `$verify` verifier) — a testkit enablement outside Phase 7b's scope"),
    // Phase 7b §16.5: the case's premise — an app pure function recomputed in a
    // database-evaluated `$view` across replay, yielding an unspecified post-restart
    // value — is superseded: an app-registered call in a view is now a load error
    // (§16.5), and in its one legal position (a mutation body) a computed value is
    // written into committed state and reused verbatim on replay (§8.12), so no
    // recomputation-divergence remains to be unspecified about. The lying-pure
    // replay concern (SPEC-ISSUES #15) needs re-authoring against a native/built-in
    // divergence or a mutation-body recorded result; the app-fn-in-view form cannot
    // survive the §16.5 position move, so it is acknowledged debt pending re-author.
    ("23-host-contract/impure-pure-function-replay-divergence-unspecified", "superseded by §16.5: an app pure function in a database-evaluated `$view` is now a load error, and in a mutation body its result is recorded (§8.12) so replay is deterministic — the recomputation-divergence premise cannot survive the position move; needs re-authoring"),
    // §18 blob views: a parameterized surface/top-level `$view` a case reads now
    // compiles and serves (adapter/surface_params.rs reconstructs its `$params`),
    // and the §18.5 placement facts are recorded into the engine before a placement
    // member is read (adapter/blobs.rs + runtime.rs, §18.5). The residuals below are
    // an aggregate-over-projected-member type-check gap or a background reconciler
    // step — neither the record-placement seam.
    //
    // The §12.2 keyed-selection surface-view shape is CLOSED as a corpus bug: a
    // keyed-selection view `.docs[@id] { … }` is a §6.3/Annex C.6 collection
    // *selector*, so it yields a ROW VIEW (zero-or-one rows for a scalar key),
    // which §12.2 delivers as `init(frontier, rows)` — an array, NOT a single
    // object (the runtime/adapter is spec-correct, matching `06-expressions/
    // selector-scalar-key-zero-or-one-row` and `adapter_view_shape`). Four blob
    // cases wrongly expected a bare object; their expectations were corrected to
    // the one-element array. Two (`metadata-only-projection-grants-no-fetch`,
    // `placement-observations-single-store`) now pass and were un-skipped. The two
    // below still fail for a DISTINCT, previously-masked seam (not view-shape).
    ("18-blobs/billing-sum-over-stored-descriptors", "§18.11 the billing view `sum(.uploads[:u | /stores['primary'] in u.file.$stored].file.$bytes)` does not type-check — the aggregate-over-projected-member seam (`in`/`sum` over the projected `.file.$stored`/`.file.$bytes` placement member), so the package does not load"),
    ("18-blobs/corrupt-copy-demoted-and-repaired", "the placement view now resolves and steps 0–2 pass; the residual is the `run_reconciler` step (a background reconciler loop over retained lineages that demotes and repairs a corrupt copy), which the single-step `reconcile`/`apply_correction` verbs do not model — the run_reconciler seam"),
    // view-shape corpus bug FIXED (array form, §6.3/§12.2); this case's residual is
    // the declared-`$name` descriptor-binding seam (same family as
    // `same-content-different-metadata-distinct-descriptors`): the honest blob
    // parameter drops the declared `$name`, so the projected `name: .file.$name`
    // member is absent from the (now correctly-array) row.
    ("18-blobs/descriptor-metadata-readable-in-view", "view-shape corpus error fixed (`.docs[@id] { … }` now expects the §6.3/§12.2 one-row array); residual is the declared-`$name` descriptor-binding seam — the honest blob parameter drops `$name`, so `name: .file.$name` is absent from the row (same seam as `same-content-different-metadata-distinct-descriptors`)"),
    // view-shape corpus bug FIXED (array form, §6.3/§12.2); this case's residual,
    // previously masked by the step-2 view-shape failure, is a §8.4/§8.5 bound-patch
    // admission seam: the `set_enabled` mutation `s = .stores[@id] { enabled = @enabled }`
    // BINDS a patch result to a local and is admission-rejected, whereas the direct
    // patch statement `.stores[@id] { enabled = @enabled }` (no local bind) is
    // admitted — isolated by probe (bind vs. no-bind is the discriminator, not the
    // param key). Not the view-shape seam; distinct §8 bound-patch investigation.
    ("18-blobs/surplus-copy-after-policy-shrinks", "view-shape corpus error fixed (`.docs[@id] { … }` now expects the §6.3/§12.2 one-row array); residual (previously masked) is a §8.4/§8.5 bound-patch seam — `s = .stores[@id] { enabled = @enabled }` binding a patch result to a local is admission-rejected while the direct patch statement is admitted (isolated by probe: bind vs no-bind, not the param key)"),
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
    ("14-buckets/short-form-from-defaults-to-created", "no view value produced (unsupported view/watch path)"),
    // --- fail:outcome ---
    // (§6.3 duplicate row-mutation-receiver occurrences now reject — the runtime
    // splits a flat receiver key into its selector operands and counts the ones
    // naming a live row, rejecting unless exactly one occurrence remains. This
    // landed `06-expressions/row-mutation-receiver-duplicate-occurrences-reject`,
    // whose entry was pruned as stale.)
    // (§10.3/§10.5 scoped-role addressing is now wired — the surface host resolves a
    // role held by a specific ROW addressed by (row identity + role name) and, under
    // `$recursive`, a covered DESCENDANT by (role handle + key path); membership is
    // re-evaluated PER SCOPE ROW. This landed `scoped-role-addressed-by-row-and-name`,
    // `recursive-descendant-mutation-addressing`, and `deleted-scope-row-revokes-role`,
    // whose entries were pruned as stale.)
    ("10-interfaces-roles/fixed-call-argument-not-overridable", "outcome divergence: expected `ok` observed `denied`"),
    ("10-interfaces-roles/row-mutation-receiver-exactly-one", "outcome divergence: expected `rejected` observed `denied`"),
    ("11-auth-sessions/login-operation-id-replay-at-most-once", "outcome divergence: expected `ok` observed `rejected`"),
    ("11-auth-sessions/login-operation-id-reuse-different-request-rejected", "outcome divergence: expected `ok` observed `rejected`"),
    ("11-auth-sessions/login-token-immediately-usable", "outcome divergence: expected `ok` observed `rejected`"),
    ("12-clients-live-views/parameter-normalization-and-checks", "outcome divergence: expected `ok` observed `denied`"),
    // (§10.1/§8.2 nested-receiver reconstruction is fixed — the harness now
    // collects every ancestor selector's params, so a depth-≥2 receiver
    // `.companies[@company].accounts[@account].consume` addresses the account by
    // its full key `[company, account]` instead of dropping `@company`. This
    // landed `hierarchical-limits-clear-every-level` and
    // `hierarchical-level-without-meter-adds-no-constraint`, whose entries were
    // pruned as stale.)
    ("23-host-contract/restart-preserves-identity-values-and-view", "outcome divergence: expected `ok` observed `denied`"),
    // --- fail:valdiff ---
    // --- fail:viewdiff ---
    // (§14.2/§8 `-.coll.$all[:x | pred]` deletion now RESOLVES its target: the
    // `-selection` delete path peels the `.$all` temporal selector and outer
    // `[selector]` to find the collection, and removes a nested collection's row by
    // its full address (`interp.rs::exec_delete_selection`/`selection_collection`).
    // Before, `collection_ref` could not resolve the `.spends.$all` base, so every
    // `-.coll.$all[…]` delete silently no-opped — the row survived. This landed
    // `15-meters/inactive-bucketed-spend-retains-allocation` (the deleted spend's
    // §15.2 allocation now releases, restoring the pool balance) and
    // `14-buckets/expiration-preserves-row-in-all` (the top-level `.$all` purge now
    // removes the extant row), whose entries were pruned as stale.)
    ("05-state-model/set-of-enum-reads-in-declaration-order", "view result diverges from expectation"),
    ("12-clients-live-views/temporal-observation-advances-live-view", "view result diverges from expectation"),
    ("12-clients-live-views/window-anchor-survives-rekey", "view result diverges from expectation"),
    ("22-runtime-semantics/concurrent-appends-either-order-both-atomic", "view result diverges from expectation"),
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
