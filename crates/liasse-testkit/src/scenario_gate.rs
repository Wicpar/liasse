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
    // --- §18 blob steps drive the composed blob host, but these do not yet pass ---
    // The blob-parameter upload (`blob_put`), fetch (`blob_get`), and connector
    // fault-injection (`connector_set`) now run against a real §18 `BlobHost`
    // composed from the case's `hosts.connectors` + `$data` stores. The residual
    // debt below is a genuine seam, not a missing driver: a §18.5 placement
    // observation (`.file.$satisfied`/`.file.$stored`) the runtime does not track on
    // a surface-bound descriptor; a declared descriptor member (`$name`, a verifying
    // `claim`) the honest `call_with_blob` blob parameter cannot bind into the call;
    // a role-scoped §18.8 fetch visibility the digest-keyed host does not resolve; or
    // a `.liasse` archive step the adapter does not link.
    ("18-blobs/accepted-upload-commits-and-verifies", "§18.5 placement observation (`.file.$satisfied`/`$stored`) not tracked on a surface-bound blob descriptor"),
    ("18-blobs/all-branch-verifies-every-copy-at-admission", "§18.5 placement observation not tracked on a surface-bound blob descriptor"),
    ("18-blobs/disabled-store-excluded-from-placement", "§18.5 placement observation not tracked on a surface-bound blob descriptor"),
    ("18-blobs/same-content-different-metadata-distinct-descriptors", "a declared `$name` must bind into the mutation call, which the honest `call_with_blob` blob parameter drops"),
    ("18-blobs/descriptor-bytes-encoding-unspecified", "a verifying client-declared descriptor must bind into the mutation call, which the honest-only blob parameter does not expose"),
    ("18-blobs/fetch-reevaluates-revoked-membership-denied", "role-scoped blob surface authorization / §18.8 fetch visibility not resolved (denied)"),
    ("18-blobs/known-hash-without-visibility-no-fetch", "role-scoped §18.8 fetch visibility over the surface projection not resolved (denied)"),
    ("23-host-contract/connector-failure-preserves-committed-state", "§18.5 placement observation not tracked on a surface-bound blob descriptor"),
    ("23-host-contract/connector-tampered-read-refetched-from-verified-holder", "role-scoped §18.8 fetch / §18.5 placement observation not resolved on a surface-bound descriptor"),
    // --- `budget_set` step ---
    ("23-host-contract/budget-backpressure-or-reject-choice-unspecified", "`budget_set` step not driven this phase"),
    ("23-host-contract/budget-exhaustion-never-partial-transition", "`budget_set` step not driven this phase"),
    // (§4 `build_artifact`/`repack_artifact`/`load_artifact` now drive the real
    // `liasse-artifact` archive layer — adapter/artifacts.rs — so every
    // 04-package-structure case passes.)
    // --- `connector_set` drives §18.12 fault injection; this residual is a seam ---
    ("18-blobs/any-branch-selects-first-fulfillable", "§18.5 placement observation not tracked on a surface-bound blob descriptor"),
    // --- `erase` step ---
    ("annex-d-identity/erase-removes-live-row-and-rechecksums-history", "`erase` step not driven this phase"),
    ("annex-d-identity/erasure-extract-replay-foreign-instance-rejected", "`erase` step not driven this phase"),
    // --- `export` step ---
    ("19-history-artifacts/displaced-lineage-reimport-is-merge", "`export` step not driven this phase"),
    ("19-history-artifacts/forged-state-consistent-checksums-unspecified", "`export` step not driven this phase"),
    ("19-history-artifacts/history-index-overlapping-ranges-invalid", "`export` step not driven this phase"),
    ("19-history-artifacts/import-fast-forward-applies-continuation", "`export` step not driven this phase"),
    ("19-history-artifacts/import-policy-gates-activation", "`export` step not driven this phase"),
    ("19-history-artifacts/import-replay-idempotent", "`export` step not driven this phase"),
    ("19-history-artifacts/imported-state-survives-restart", "`export` step not driven this phase"),
    ("19-history-artifacts/manifest-extra-member-invalid", "`export` step not driven this phase"),
    ("19-history-artifacts/manifest-index-selection-mismatch-invalid", "`export` step not driven this phase"),
    ("19-history-artifacts/merge-combined-uniqueness-violation-conflict", "`export` step not driven this phase"),
    ("19-history-artifacts/merge-compatible-separate-coordinates", "`export` step not driven this phase"),
    ("19-history-artifacts/merge-competing-module-mounts-conflict", "`export` step not driven this phase"),
    ("19-history-artifacts/merge-correction-activates-new-lineage", "`export` step not driven this phase"),
    ("19-history-artifacts/mimetype-mismatch-invalid", "`export` step not driven this phase"),
    ("19-history-artifacts/missing-required-entry-invalid", "`export` step not driven this phase"),
    ("19-history-artifacts/point-id-aliasing-unrelated-history-unspecified", "`export` step not driven this phase"),
    ("19-history-artifacts/restore-reexport-preserves-selection-identity", "`export` step not driven this phase"),
    ("19-history-artifacts/spliced-state-selection-mismatch-invalid", "`export` step not driven this phase"),
    ("19-history-artifacts/tampered-state-checksum-mismatch-invalid", "`export` step not driven this phase"),
    ("19-history-artifacts/unknown-extra-entry-unspecified", "`export` step not driven this phase"),
    ("19-history-artifacts/zip-path-traversal-entry-invalid", "`export` step not driven this phase"),
    ("20-evolution-migrations/downgrade-preserves-history-order", "`export` step not driven this phase"),
    ("annex-d-identity/display-path-key-slash-escaped-in-correction", "`export` step not driven this phase"),
    // --- `host_load` step ---
    ("20-evolution-migrations/confusable-from-source-name-nonexistent-rejected", "`host_load` step not driven this phase"),
    ("20-evolution-migrations/downgrade-drops-unrepresentable-field-rejected", "`host_load` step not driven this phase"),
    ("20-evolution-migrations/downgrade-via-inverse-restores-value", "`host_load` step not driven this phase"),
    ("20-evolution-migrations/empty-source-collection-migrates-to-empty", "`host_load` step not driven this phase"),
    ("20-evolution-migrations/migrated-state-dangling-ref-rejected", "`host_load` step not driven this phase"),
    ("20-evolution-migrations/migration-order-from-before-program", "`host_load` step not driven this phase"),
    ("20-evolution-migrations/migration-program-atomicity-partial-failure-rejected", "`host_load` step not driven this phase"),
    ("20-evolution-migrations/migration-program-key-collision-rejected", "`host_load` step not driven this phase"),
    ("20-evolution-migrations/migration-program-splits-collection", "`host_load` step not driven this phase"),
    ("20-evolution-migrations/minor-update-narrowing-despite-migration-rejected", "`host_load` step not driven this phase"),
    ("20-evolution-migrations/missing-migration-for-active-source-version-rejected", "`host_load` step not driven this phase"),
    ("20-evolution-migrations/replay-identical-version-update-unchanged", "`host_load` step not driven this phase"),
    ("20-evolution-migrations/reversible-transform-roundtrip-commits", "`host_load` step not driven this phase"),
    ("annex-e-compatibility/downgrade-drops-populated-field-rejected", "`host_load` step not driven this phase"),
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
    // `$config` (§13.1): recorded on the install request, but neither type-checked
    // against the declared struct nor readable through child expressions.
    ("13-modules/install-config-type-mismatch-invalid", "§13.1 installation `$config` is recorded but not type-checked against the child's declared `$config` struct, so a type mismatch is admitted"),
    ("13-modules/install-config-unknown-member-invalid", "§13.1/§2.5 installation `$config` is recorded but its members are not validated against the declared struct, so an unknown member is admitted"),
    ("13-modules/module-config-values-read-through-binding", "§13.1 `$config` read-through in child expressions is unlanded (the expression language has no `$config` binding), so the child fails to compile at install"),
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
    ("04-package-structure/data-expression-and-literal-escape", "host environment row-field shaping gap (engine invariant)"),
    ("05-state-model/named-type-recursive-shape", "host environment row-field shaping gap (engine invariant)"),
    ("06-expressions/caret-reads-lexical-parent-scope", "host environment row-field shaping gap (engine invariant)"),
    ("06-expressions/selector-set-keys-follow-target-canonical-order", "host environment row-field shaping gap (engine invariant)"),
    ("annex-c-grammar/computed-field-equals-prefix-form", "host environment row-field shaping gap (engine invariant)"),
    ("annex-c-grammar/deep-nested-projection-loads", "host environment row-field shaping gap (engine invariant)"),
    ("annex-c-grammar/double-leading-quote-stores-single-quote", "host environment row-field shaping gap (engine invariant)"),
    ("annex-c-grammar/literal-equals-prefixed-text-escaped-in-data", "host environment row-field shaping gap (engine invariant)"),
    ("annex-c-grammar/literal-leading-quote-removed-in-data", "host environment row-field shaping gap (engine invariant)"),
    ("annex-c-grammar/lone-quote-in-data-stores-empty-string", "host environment row-field shaping gap (engine invariant)"),
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
    ("14-buckets/between-window-spanning-rollover-returns-both-periods", "package does not load yet (upstream compile/model gap)"),
    ("14-buckets/calendar-monthly-clamp-preserves-anchor", "package does not load yet (upstream compile/model gap)"),
    ("14-buckets/dst-fall-back-ambiguous-earlier", "package does not load yet (upstream compile/model gap)"),
    ("14-buckets/recurring-rollover-at-exact-boundary", "package does not load yet (upstream compile/model gap)"),
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
    ("18-blobs/all-holders-corrupt-fetch-outcome-unspecified", "package does not load yet (upstream compile/model gap)"),
    ("18-blobs/billing-sum-over-stored-descriptors", "package does not load yet (upstream compile/model gap)"),
    ("18-blobs/corrupt-copy-demoted-and-repaired", "package does not load yet (upstream compile/model gap)"),
    ("18-blobs/descriptor-metadata-readable-in-view", "package does not load yet (upstream compile/model gap)"),
    ("18-blobs/fetch-returns-exact-bytes", "package does not load yet (upstream compile/model gap)"),
    ("18-blobs/live-descriptor-pins-content-across-restart", "package does not load yet (upstream compile/model gap)"),
    ("18-blobs/metadata-only-projection-grants-no-fetch", "package does not load yet (upstream compile/model gap)"),
    ("18-blobs/placement-observations-single-store", "package does not load yet (upstream compile/model gap)"),
    ("18-blobs/repeated-store-identity-deduplicated", "package does not load yet (upstream compile/model gap)"),
    ("18-blobs/serve-order-defaults-to-flattened-placement", "package does not load yet (upstream compile/model gap)"),
    ("18-blobs/surplus-copy-after-policy-shrinks", "package does not load yet (upstream compile/model gap)"),
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
    ("15-meters/w3-overlapping-heterogeneous-credits", "seed admission gap: seed field `period`: `duration` value `= none` is not a canonical ISO-8601 elapsed dura..."),
    ("annex-a-types-wire/ref-composite-wire-is-key-order-array", "seed admission gap: reference `loc` does not resolve to a live row"),
    // ========================================================================
    // RUNTIME RESULT DIVERGES FROM THE CORPUS EXPECTATION
    // The package loads and the steps run, but the runtime's observed outcome, value,
    // or view result does not yet match what the case (re-derived from SPEC.md)
    // expects. Each is real conformance debt for the triage loop; none is edited away.
    // ========================================================================
    // --- fail:noval ---
    ("08-mutations-validation/inferred-param-target-normalization-applies", "no value produced (unsupported call path)"),
    ("15-meters/hypothetical-balance-accessor-with-time", "no value produced (unsupported call path)"),
    ("21-deletion-erasure/double-reinsert-second-finds-no-stub-rejects", "no value produced (unsupported call path)"),
    ("21-deletion-erasure/erase-cascade-scrub-scope-unspecified", "no value produced (unsupported call path)"),
    ("21-deletion-erasure/erase-removes-row-from-live-state", "no value produced (unsupported call path)"),
    ("21-deletion-erasure/erased-row-absent-across-export-restore", "no value produced (unsupported call path)"),
    ("21-deletion-erasure/reinsert-historical-does-not-recreate-live-row", "no value produced (unsupported call path)"),
    ("21-deletion-erasure/reinsert-tampered-extract-hash-rejects", "no value produced (unsupported call path)"),
    ("annex-c-grammar/mutation-name-explicit-prototype-parses", "no value produced (unsupported call path)"),
    ("annex-c-grammar/noparam-call-paren-equals-empty-args", "no value produced (unsupported call path)"),
    ("annex-c-grammar/set-field-form-add-and-remove-members", "no value produced (unsupported call path)"),
    // --- fail:noview ---
    ("10-interfaces-roles/except-prunes-entire-branch", "no view value produced (unsupported view/watch path)"),
    ("10-interfaces-roles/recursive-coverage-nests-included-descendants", "no view value produced (unsupported view/watch path)"),
    ("14-buckets/short-form-from-defaults-to-created", "no view value produced (unsupported view/watch path)"),
    // --- fail:outcome ---
    ("05-state-model/nested-initializer-failure-rejects-parent-insert", "outcome divergence: expected `ok` observed `rejected`"),
    ("06-expressions/row-mutation-receiver-duplicate-occurrences-reject", "outcome divergence: expected `ok` observed `rejected`"),
    ("10-interfaces-roles/deleted-scope-row-revokes-role", "outcome divergence: expected `ok` observed `denied`"),
    ("10-interfaces-roles/fixed-call-argument-not-overridable", "outcome divergence: expected `ok` observed `denied`"),
    ("10-interfaces-roles/membership-reevaluated-each-admission", "outcome divergence: expected `ok` observed `rejected`"),
    ("10-interfaces-roles/recursive-descendant-mutation-addressing-unspecified", "outcome divergence: expected `ok` observed `denied`"),
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
    ("21-deletion-erasure/erased-row-unobservable-in-second-view", "view result diverges from expectation"),
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
