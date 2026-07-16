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
    ("10-interfaces-roles/role-rejects-unaccepted-authenticator", "`authenticate` step not driven this phase"),
    ("12-clients-live-views/shared-connection-cross-context-isolation", "`authenticate` step not driven this phase"),
    // --- `blob_put` step ---
    ("18-blobs/accepted-upload-commits-and-verifies", "`blob_put` step not driven this phase"),
    ("18-blobs/all-branch-verifies-every-copy-at-admission", "`blob_put` step not driven this phase"),
    ("18-blobs/artifact-blob-inclusion-selection-unspecified", "`blob_put` step not driven this phase"),
    ("18-blobs/at-max-bytes-boundary-accepted", "`blob_put` step not driven this phase"),
    ("18-blobs/claimed-byte-count-mismatch-rejected", "`blob_put` step not driven this phase"),
    ("18-blobs/confusable-media-type-not-accepted", "`blob_put` step not driven this phase"),
    ("18-blobs/descriptor-bytes-encoding-unspecified", "`blob_put` step not driven this phase"),
    ("18-blobs/disabled-store-excluded-from-placement", "`blob_put` step not driven this phase"),
    ("18-blobs/fetch-reevaluates-revoked-membership-denied", "`blob_put` step not driven this phase"),
    ("18-blobs/known-hash-without-visibility-no-fetch", "`blob_put` step not driven this phase"),
    ("18-blobs/max-bytes-boundary-plus-one-rejected", "`blob_put` step not driven this phase"),
    ("18-blobs/media-compared-case-insensitively", "`blob_put` step not driven this phase"),
    ("18-blobs/media-declaration-without-params-accepts-any-params", "`blob_put` step not driven this phase"),
    ("18-blobs/media-parameter-mismatch-rejected", "`blob_put` step not driven this phase"),
    ("18-blobs/media-parameter-reordering-accepted", "`blob_put` step not driven this phase"),
    ("18-blobs/negative-byte-count-rejected", "`blob_put` step not driven this phase"),
    ("18-blobs/same-content-different-metadata-distinct-descriptors", "`blob_put` step not driven this phase"),
    ("18-blobs/unaccepted-media-rejected", "`blob_put` step not driven this phase"),
    ("18-blobs/uppercase-hex-sha512-handling-unspecified", "`blob_put` step not driven this phase"),
    ("18-blobs/zero-byte-blob-accepted", "`blob_put` step not driven this phase"),
    ("23-host-contract/connector-failure-preserves-committed-state", "`blob_put` step not driven this phase"),
    ("23-host-contract/connector-tampered-read-refetched-from-verified-holder", "`blob_put` step not driven this phase"),
    // --- `budget_set` step ---
    ("23-host-contract/budget-backpressure-or-reject-choice-unspecified", "`budget_set` step not driven this phase"),
    ("23-host-contract/budget-exhaustion-never-partial-transition", "`budget_set` step not driven this phase"),
    // --- `build_artifact` step ---
    ("04-package-structure/definition-swap-after-build-rejected", "`build_artifact` step not driven this phase"),
    ("04-package-structure/duplicate-archive-entry-rejected", "`build_artifact` step not driven this phase"),
    ("04-package-structure/duplicate-resource-entry-rejected", "`build_artifact` step not driven this phase"),
    ("04-package-structure/liasse-json-duplicate-member-rejected", "`build_artifact` step not driven this phase"),
    ("04-package-structure/manifest-checksum-mismatch-rejected", "`build_artifact` step not driven this phase"),
    ("04-package-structure/manifest-unknown-member-rejected", "`build_artifact` step not driven this phase"),
    ("04-package-structure/resource-bytes-tampered-digest-mismatch-rejected", "`build_artifact` step not driven this phase"),
    ("04-package-structure/resource-digest-mismatch-rejected", "`build_artifact` step not driven this phase"),
    ("04-package-structure/resource-digest-verified-at-load", "`build_artifact` step not driven this phase"),
    // --- `connector_set` step ---
    ("18-blobs/any-branch-selects-first-fulfillable", "`connector_set` step not driven this phase"),
    ("18-blobs/copies-fewer-than-n-rejected", "`connector_set` step not driven this phase"),
    // --- `erase` step ---
    ("annex-d-identity/erase-removes-live-row-and-rechecksums-history", "`erase` step not driven this phase"),
    ("annex-d-identity/erasure-extract-replay-foreign-instance-rejected", "`erase` step not driven this phase"),
    // --- `export` step ---
    ("19-history-artifacts/displaced-lineage-reimport-is-merge", "`export` step not driven this phase"),
    ("19-history-artifacts/duplicate-archive-entry-invalid", "`export` step not driven this phase"),
    ("19-history-artifacts/export-artifact-structure-and-mimetype", "`export` step not driven this phase"),
    ("19-history-artifacts/export-boundary-atomic-under-concurrent-write", "`export` step not driven this phase"),
    ("19-history-artifacts/export-boundary-excludes-later-commits", "`export` step not driven this phase"),
    ("19-history-artifacts/forged-state-consistent-checksums-unspecified", "`export` step not driven this phase"),
    ("19-history-artifacts/history-index-genesis-lineage", "`export` step not driven this phase"),
    ("19-history-artifacts/history-index-overlapping-ranges-invalid", "`export` step not driven this phase"),
    ("19-history-artifacts/import-fast-forward-applies-continuation", "`export` step not driven this phase"),
    ("19-history-artifacts/import-policy-gates-activation", "`export` step not driven this phase"),
    ("19-history-artifacts/import-replay-idempotent", "`export` step not driven this phase"),
    ("19-history-artifacts/import-rollback-restores-earlier-point", "`export` step not driven this phase"),
    ("19-history-artifacts/import-same-point-already-synchronized", "`export` step not driven this phase"),
    ("19-history-artifacts/imported-state-survives-restart", "`export` step not driven this phase"),
    ("19-history-artifacts/manifest-entries-cover-required-entries", "`export` step not driven this phase"),
    ("19-history-artifacts/manifest-extra-member-invalid", "`export` step not driven this phase"),
    ("19-history-artifacts/manifest-included-range-statement-unspecified", "`export` step not driven this phase"),
    ("19-history-artifacts/manifest-index-selection-mismatch-invalid", "`export` step not driven this phase"),
    ("19-history-artifacts/merge-combined-uniqueness-violation-conflict", "`export` step not driven this phase"),
    ("19-history-artifacts/merge-compatible-separate-coordinates", "`export` step not driven this phase"),
    ("19-history-artifacts/merge-competing-inserts-same-key-conflict", "`export` step not driven this phase"),
    ("19-history-artifacts/merge-competing-module-mounts-conflict", "`export` step not driven this phase"),
    ("19-history-artifacts/merge-conflict-leaves-state-unchanged", "`export` step not driven this phase"),
    ("19-history-artifacts/merge-correction-activates-new-lineage", "`export` step not driven this phase"),
    ("19-history-artifacts/merge-delete-vs-modify-conflict", "`export` step not driven this phase"),
    ("19-history-artifacts/merge-equal-inserts-incarnation-unspecified", "`export` step not driven this phase"),
    ("19-history-artifacts/merge-equal-results-both-sides", "`export` step not driven this phase"),
    ("19-history-artifacts/mimetype-mismatch-invalid", "`export` step not driven this phase"),
    ("19-history-artifacts/missing-required-entry-invalid", "`export` step not driven this phase"),
    ("19-history-artifacts/point-id-aliasing-unrelated-history-unspecified", "`export` step not driven this phase"),
    ("19-history-artifacts/restore-reexport-preserves-selection-identity", "`export` step not driven this phase"),
    ("19-history-artifacts/restore-round-trip-reproduces-state", "`export` step not driven this phase"),
    ("19-history-artifacts/rollback-preserves-displaced-future-lineage", "`export` step not driven this phase"),
    ("19-history-artifacts/spliced-state-selection-mismatch-invalid", "`export` step not driven this phase"),
    ("19-history-artifacts/tampered-state-checksum-mismatch-invalid", "`export` step not driven this phase"),
    ("19-history-artifacts/unchanged-mutation-creates-no-history-point", "`export` step not driven this phase"),
    ("19-history-artifacts/unicode-confusable-keys-roundtrip-distinct", "`export` step not driven this phase"),
    ("19-history-artifacts/unknown-extra-entry-unspecified", "`export` step not driven this phase"),
    ("19-history-artifacts/zip-path-traversal-entry-invalid", "`export` step not driven this phase"),
    ("20-evolution-migrations/downgrade-preserves-history-order", "`export` step not driven this phase"),
    ("21-deletion-erasure/deleted-values-remain-in-history", "`export` step not driven this phase"),
    ("annex-d-identity/definition-identity-independent-of-selected-state", "`export` step not driven this phase"),
    ("annex-d-identity/display-path-key-slash-escaped-in-correction", "`export` step not driven this phase"),
    ("annex-d-identity/duplicate-entry-breaks-exists-exactly-once", "`export` step not driven this phase"),
    ("annex-d-identity/liasse-json-byte-tamper-fails-entry-checksum", "`export` step not driven this phase"),
    ("annex-d-identity/liasse-json-swap-with-fixed-checksums-stale-identity-unspecified", "`export` step not driven this phase"),
    // --- `host_load` step ---
    ("09-loading-bootstrap/rejected-update-leaves-prior-active", "`host_load` step not driven this phase"),
    ("09-loading-bootstrap/update-added-check-rejected-against-existing-state", "`host_load` step not driven this phase"),
    ("09-loading-bootstrap/update-missing-namespace-keeps-prior-active", "`host_load` step not driven this phase"),
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
    ("20-evolution-migrations/rename-collection-via-from", "`host_load` step not driven this phase"),
    ("20-evolution-migrations/replay-identical-version-update-unchanged", "`host_load` step not driven this phase"),
    ("20-evolution-migrations/reversible-transform-roundtrip-commits", "`host_load` step not driven this phase"),
    ("annex-e-compatibility/downgrade-drops-populated-field-rejected", "`host_load` step not driven this phase"),
    ("annex-e-compatibility/enum-result-confusable-label-swap-rejected", "`host_load` step not driven this phase"),
    ("annex-e-compatibility/exposed-view-identity-change-rejected", "`host_load` step not driven this phase"),
    ("annex-e-compatibility/minor-adds-required-parameter-rejected", "`host_load` step not driven this phase"),
    ("annex-e-compatibility/minor-makes-required-output-optional-rejected", "`host_load` step not driven this phase"),
    ("annex-e-compatibility/minor-narrows-input-enum-domain-rejected", "`host_load` step not driven this phase"),
    ("annex-e-compatibility/minor-narrows-relative-to-active-intermediate-release-rejected", "`host_load` step not driven this phase"),
    ("annex-e-compatibility/minor-removes-output-member-rejected", "`host_load` step not driven this phase"),
    ("annex-e-compatibility/patch-narrowing-response-rejected", "`host_load` step not driven this phase"),
    // --- `keyring_admin` step ---
    ("17-keyrings/bind-algorithm-mismatch-rejected", "`keyring_admin` step not driven this phase"),
    ("17-keyrings/manual-activation-enables-dependent-surface", "`keyring_admin` step not driven this phase"),
    ("17-keyrings/manual-second-activation-retires-prior", "`keyring_admin` step not driven this phase"),
    // --- `manifest` step ---
    ("12-clients-live-views/manifest-lists-granted-surfaces", "`manifest` step not driven this phase"),
    // --- `module_install` step ---
    ("13-modules/aggregation-skips-disabled-instance", "`module_install` step not driven this phase"),
    ("13-modules/cross-boundary-ref-missing-on-delete-invalid", "`module_install` step not driven this phase"),
    ("13-modules/cross-module-atomic-transition", "`module_install` step not driven this phase"),
    ("13-modules/empty-instance-name-invalid", "`module_install` step not driven this phase"),
    ("13-modules/explicit-peer-binding-cross-space-rejected", "`module_install` step not driven this phase"),
    ("13-modules/expose-binding-contract-mismatch-invalid", "`module_install` step not driven this phase"),
    ("13-modules/expose-view-missing-interface-field-invalid", "`module_install` step not driven this phase"),
    ("13-modules/if-module-guarded-state-preserved", "`module_install` step not driven this phase"),
    ("13-modules/install-config-type-mismatch-invalid", "`module_install` step not driven this phase"),
    ("13-modules/install-config-unknown-member-invalid", "`module_install` step not driven this phase"),
    ("13-modules/install-data-check-failure-rejected", "`module_install` step not driven this phase"),
    ("13-modules/install-data-overlay-merge", "`module_install` step not driven this phase"),
    ("13-modules/install-duplicate-instance-name-rejected", "`module_install` step not driven this phase"),
    ("13-modules/install-into-nonexistent-space-rejected", "`module_install` step not driven this phase"),
    ("13-modules/interface-aggregation-inherited-identity", "`module_install` step not driven this phase"),
    ("13-modules/interface-mutation-contract-shapes", "`module_install` step not driven this phase"),
    ("13-modules/interface-mutation-param-contract-mismatch-invalid", "`module_install` step not driven this phase"),
    ("13-modules/minor-update-narrowing-rejected", "`module_install` step not driven this phase"),
    ("13-modules/module-config-values-read-through-binding", "`module_install` step not driven this phase"),
    ("13-modules/module-space-instances-independent", "`module_install` step not driven this phase"),
    ("13-modules/optional-peer-absent-valid", "`module_install` step not driven this phase"),
    ("13-modules/parent-exposed-surface-row-local", "`module_install` step not driven this phase"),
    ("13-modules/peer-incompatible-major-rejected", "`module_install` step not driven this phase"),
    ("13-modules/peer-lookup-stays-in-sibling-space-rejected", "`module_install` step not driven this phase"),
    ("13-modules/peer-multiple-candidates-need-explicit-binding", "`module_install` step not driven this phase"),
    ("13-modules/peer-single-candidate-auto-binds", "`module_install` step not driven this phase"),
    ("13-modules/peer-zero-candidates-rejected", "`module_install` step not driven this phase"),
    ("13-modules/private-deps-isolated-per-consumer", "`module_install` step not driven this phase"),
    ("13-modules/rename-instance-stale-name-not-addressable", "`module_install` step not driven this phase"),
    ("13-modules/required-peer-disabled-candidate-rejected", "`module_install` step not driven this phase"),
    ("13-modules/sibling-cannot-address-private-dep-rejected", "`module_install` step not driven this phase"),
    ("13-modules/unexposed-parent-authenticator-invalid", "`module_install` step not driven this phase"),
    ("13-modules/uninstall-blocked-by-cross-boundary-restrict-ref", "`module_install` step not driven this phase"),
    ("13-modules/uninstall-unbinds-optional-peer", "`module_install` step not driven this phase"),
    ("13-modules/update-narrowing-view-field-rejected", "`module_install` step not driven this phase"),
    ("13-modules/update-result-report", "`module_install` step not driven this phase"),
    ("13-modules/update-seed-three-way-merge", "`module_install` step not driven this phase"),
    ("19-history-artifacts/child-export-matches-embedded-artifact", "`module_install` step not driven this phase"),
    ("19-history-artifacts/child-module-artifact-embedded-and-extractable", "`module_install` step not driven this phase"),
    ("19-history-artifacts/tampered-child-artifact-invalid", "`module_install` step not driven this phase"),
    ("annex-e-compatibility/module-minor-rebinds-interface-implementation-accepted", "`module_install` step not driven this phase"),
    ("annex-e-compatibility/module-removes-interface-binding-rejected", "`module_install` step not driven this phase"),
    ("w-worked-examples/w4-confusable-instance-names-both-install-distinct", "`module_install` step not driven this phase"),
    ("w-worked-examples/w4-host-imports-exposed-template-across-boundary", "`module_install` step not driven this phase"),
    ("w-worked-examples/w4-import-disabled-template-across-boundary-rejected", "`module_install` step not driven this phase"),
    ("w-worked-examples/w4-install-exposes-enabled-template-to-parent", "`module_install` step not driven this phase"),
    ("w-worked-examples/w4-plan-gates-template-disabled-not-exposed", "`module_install` step not driven this phase"),
    ("w-worked-examples/w4-seed-computed-enabled-reevaluation-unspecified", "`module_install` step not driven this phase"),
    ("w-worked-examples/w4-uninstall-removes-aggregated-templates", "`module_install` step not driven this phase"),
    // --- `operation_status` step ---
    ("12-clients-live-views/operation-status-identifier-is-capability", "`operation_status` step not driven this phase"),
    ("12-clients-live-views/operation-status-reports-committed", "`operation_status` step not driven this phase"),
    // --- `operator` step ---
    // Root-mutation operator transitions now drive through a synthetic public
    // surface (`SurfaceHost::operator_call`); the entries below remain debt for a
    // distinct reason the operator wiring does not resolve.
    ("23-host-contract/operator-bypasses-role-authentication", "role-authenticated client call is denied without host `$verify` wiring"),
    ("23-host-contract/operator-retains-meter-capacity", "`operator` on a collection-row mutation needs receiver-row wiring"),
    ("23-host-contract/operator-transition-retains-checks", "operator transition executes; `qty` check value still diverges"),
    ("23-host-contract/provider-error-rejects-preserving-committed-state", "`keyring_admin`/`provider_set` steps not driven this phase"),
    // --- `provider_set` step ---
    ("17-keyrings/sign-failure-rejects-mutation-without-effect", "`provider_set` step not driven this phase"),
    ("23-host-contract/no-time-budget-hanging-provider-unspecified", "`provider_set` step not driven this phase"),
    // --- `restart` step ---
    ("08-mutations-validation/generated-values-recorded-survive-restart", "engine restart/rebuild-from-store not exposed by the surface host"),
    ("09-loading-bootstrap/genesis-generated-values-stable-across-restart", "engine restart/rebuild-from-store not exposed by the surface host"),
    ("09-loading-bootstrap/genesis-state-survives-restart", "engine restart/rebuild-from-store not exposed by the surface host"),
    ("14-buckets/restart-preserves-recorded-created-interval", "engine restart/rebuild-from-store not exposed by the surface host"),
    ("22-runtime-semantics/committed-state-durable-across-restart", "engine restart/rebuild-from-store not exposed by the surface host"),
    ("22-runtime-semantics/now-recorded-once-survives-restart", "engine restart/rebuild-from-store not exposed by the surface host"),
    ("22-runtime-semantics/recorded-now-not-rerolled-after-clock-advance-and-restart", "engine restart/rebuild-from-store not exposed by the surface host"),
    ("22-runtime-semantics/rejected-partial-write-leaves-no-trace-after-restart", "engine restart/rebuild-from-store not exposed by the surface host"),
    ("annex-b-total-order/all-equal-sort-key-falls-to-row-identity", "engine restart/rebuild-from-store not exposed by the surface host"),
    // --- `resume` step ---
    ("12-clients-live-views/resume-from-retained-frontier", "`resume` step not driven this phase"),
    ("12-clients-live-views/resume-with-foreign-frontier", "`resume` step not driven this phase"),
    // ========================================================================
    // HOST-ENVIRONMENT SHAPING INVARIANTS (ENGINE)
    // The step reached the engine but tripped an engine invariant while shaping the
    // evaluation environment (a row/collection the runtime could not present in the
    // expected shape). A runtime gap, surfaced as a host fault and skipped.
    // ========================================================================
    // --- hostfault:conn ---
    ("17-keyrings/cross-authenticator-token-escalation-denied", "connection-lifecycle gap: named connection not open"),
    ("17-keyrings/direct-token-roundtrip-authenticates", "connection-lifecycle gap: named connection not open"),
    ("17-keyrings/retain-boundary-instant-unspecified", "connection-lifecycle gap: named connection not open"),
    ("17-keyrings/retain-window-bounds-retired-acceptance", "connection-lifecycle gap: named connection not open"),
    ("17-keyrings/stolen-token-survives-rotations-until-session-revoked", "connection-lifecycle gap: named connection not open"),
    ("17-keyrings/wrong-keyring-token-denied", "connection-lifecycle gap: named connection not open"),
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
    // --- hostfault:scalar-card ---
    ("06-expressions/selector-scalar-key-zero-or-one-row", "scalar row-selector cardinality gap (engine invariant)"),
    ("07-views/ref-dereference-yields-target-row", "scalar row-selector cardinality gap (engine invariant)"),
    // ========================================================================
    // PACKAGE DOES NOT LOAD YET (UPSTREAM COMPILE/MODEL GAP)
    // The definition fails static validation or seed admission, so the case never
    // reaches its scenario steps. The corpus expects it to load and run; closing the
    // gap lives in the model/compile/seed layers below the runner.
    // ========================================================================
    // --- load ---
    ("07-views/conditional-false-branch-yields-empty", "package does not load yet (upstream compile/model gap)"),
    ("10-interfaces-roles/duplicate-membership-no-extra-authority", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/ambiguous-session-resolution-denied", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/authenticated-call-resolves-actor", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/committed-request-final-after-revocation", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/completion-barrier-spans-sessions", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/cross-authenticator-proof-binding-denied", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/disabled-account-fails-role-admission", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/expired-session-denied", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/explicit-authenticator-selection-required", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/forged-token-fails-verification", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/internal-call-preserves-actor", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/member-token-cannot-reach-admin-surface-denied", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/membership-reevaluated-at-admission", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/multiple-credentials-one-connection", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/public-surface-authenticator-selection-unspecified", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/revoked-session-check-denied", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/role-auth-list-accepts-any-listed", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/session-bucket-authoritative-over-token-claim", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/session-expiry-half-open-boundary", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/session-not-yet-active-denied", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/session-state-survives-restart", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/unauthenticated-role-surface-denied", "package does not load yet (upstream compile/model gap)"),
    ("11-auth-sessions/undeclared-authenticator-name-denied", "package does not load yet (upstream compile/model gap)"),
    ("14-buckets/between-intersects-half-open", "package does not load yet (upstream compile/model gap)"),
    ("14-buckets/between-window-spanning-rollover-returns-both-periods", "package does not load yet (upstream compile/model gap)"),
    ("14-buckets/calendar-monthly-clamp-preserves-anchor", "package does not load yet (upstream compile/model gap)"),
    ("14-buckets/clipped-final-interval-upper-bound-excluded", "package does not load yet (upstream compile/model gap)"),
    ("14-buckets/dst-fall-back-ambiguous-earlier", "package does not load yet (upstream compile/model gap)"),
    ("14-buckets/omitted-until-is-unbounded", "package does not load yet (upstream compile/model gap)"),
    ("14-buckets/overflow-reject-detection-timing-unspecified", "package does not load yet (upstream compile/model gap)"),
    ("14-buckets/plan-period-escalation-rejected", "package does not load yet (upstream compile/model gap)"),
    ("14-buckets/recurring-clipped-final-interval-included", "package does not load yet (upstream compile/model gap)"),
    ("14-buckets/recurring-consecutive-periods", "package does not load yet (upstream compile/model gap)"),
    ("14-buckets/recurring-rollover-at-exact-boundary", "package does not load yet (upstream compile/model gap)"),
    ("14-buckets/repeat-none-yields-single-interval", "package does not load yet (upstream compile/model gap)"),
    ("14-buckets/series-bound-equal-to-start-rejected", "package does not load yet (upstream compile/model gap)"),
    ("14-buckets/source-backed-single-interval-bindings", "package does not load yet (upstream compile/model gap)"),
    ("14-buckets/unbounded-until-sorts-after-finite", "package does not load yet (upstream compile/model gap)"),
    ("14-buckets/zero-period-rejects-source-transition", "package does not load yet (upstream compile/model gap)"),
    ("16-host-namespaces/generated-default-fixed-and-recorded", "package does not load yet (upstream compile/model gap)"),
    ("16-host-namespaces/pinned-descriptor-drift-fails-reopen", "package does not load yet (upstream compile/model gap)"),
    ("16-host-namespaces/required-namespace-pure-function-runs-in-view", "package does not load yet (upstream compile/model gap)"),
    ("16-host-namespaces/required-namespace-removed-fails-reopen", "package does not load yet (upstream compile/model gap)"),
    ("17-keyrings/automatic-bootstrap-activates-first-version", "package does not load yet (upstream compile/model gap)"),
    ("17-keyrings/destroyed-version-no-longer-accepted", "package does not load yet (upstream compile/model gap)"),
    ("17-keyrings/due-rotation-commits-once-under-concurrency", "package does not load yet (upstream compile/model gap)"),
    ("17-keyrings/key-versions-survive-restart", "package does not load yet (upstream compile/model gap)"),
    ("17-keyrings/pending-version-acceptance-during-overlap-unspecified", "package does not load yet (upstream compile/model gap)"),
    ("17-keyrings/retain-omitted-accepts-until-revoked", "package does not load yet (upstream compile/model gap)"),
    ("17-keyrings/revocation-overrides-retain-window", "package does not load yet (upstream compile/model gap)"),
    ("17-keyrings/rotation-failure-keeps-current-active", "package does not load yet (upstream compile/model gap)"),
    ("17-keyrings/rotation-overlap-exposes-pending-version", "package does not load yet (upstream compile/model gap)"),
    ("17-keyrings/scheduled-rotation-retires-prior-active", "package does not load yet (upstream compile/model gap)"),
    ("17-keyrings/shared-provider-keyrings-rotate-independently", "package does not load yet (upstream compile/model gap)"),
    ("17-keyrings/verification-survives-provider-outage", "package does not load yet (upstream compile/model gap)"),
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
    ("22-runtime-semantics/optimistic-version-assert-prevents-lost-update", "package does not load yet (upstream compile/model gap)"),
    ("23-host-contract/impure-pure-function-replay-divergence-unspecified", "package does not load yet (upstream compile/model gap)"),
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
    ("08-mutations-validation/empty-args-call-forms-equivalent", "no value produced (unsupported call path)"),
    ("08-mutations-validation/inferred-param-target-normalization-applies", "no value produced (unsupported call path)"),
    ("10-interfaces-roles/surface-exposes-only-declared-members", "no value produced (unsupported call path)"),
    ("15-meters/hypothetical-balance-accessor-with-time", "no value produced (unsupported call path)"),
    ("16-host-namespaces/rejected-update-preserves-active-composition", "no value produced (unsupported call path)"),
    ("21-deletion-erasure/double-reinsert-second-finds-no-stub-rejects", "no value produced (unsupported call path)"),
    ("21-deletion-erasure/erase-cascade-scrub-scope-unspecified", "no value produced (unsupported call path)"),
    ("21-deletion-erasure/erase-removes-row-from-live-state", "no value produced (unsupported call path)"),
    ("21-deletion-erasure/erased-row-absent-across-export-restore", "no value produced (unsupported call path)"),
    ("21-deletion-erasure/reinsert-historical-does-not-recreate-live-row", "no value produced (unsupported call path)"),
    ("21-deletion-erasure/reinsert-tampered-extract-hash-rejects", "no value produced (unsupported call path)"),
    ("annex-c-grammar/mutation-name-explicit-prototype-parses", "no value produced (unsupported call path)"),
    ("annex-c-grammar/noparam-call-paren-equals-empty-args", "no value produced (unsupported call path)"),
    ("annex-c-grammar/set-field-form-add-and-remove-members", "no value produced (unsupported call path)"),
    ("annex-d-identity/percent-escape-not-double-encoded", "no value produced (unsupported call path)"),
    ("annex-d-identity/seed-composite-key-joined-in-key-order", "no value produced (unsupported call path)"),
    ("annex-d-identity/seed-key-text-escapes-slash-and-colon", "no value produced (unsupported call path)"),
    // --- fail:noview ---
    ("10-interfaces-roles/except-prunes-entire-branch", "no view value produced (unsupported view/watch path)"),
    ("10-interfaces-roles/recursive-coverage-nests-included-descendants", "no view value produced (unsupported view/watch path)"),
    ("10-interfaces-roles/surface-params-defaults-apply", "no view value produced (unsupported view/watch path)"),
    ("10-interfaces-roles/view-filter-does-not-gate-receiver", "no view value produced (unsupported view/watch path)"),
    ("14-buckets/backdated-interval-rewrites-at-window", "no view value produced (unsupported view/watch path)"),
    ("14-buckets/short-form-from-defaults-to-created", "no view value produced (unsupported view/watch path)"),
    ("15-meters/overlapping-credits-total-exhaustion-rejects", "no view value produced (unsupported view/watch path)"),
    ("16-host-namespaces/verifier-namespace-runs-at-admission", "no view value produced (unsupported view/watch path)"),
    ("w-worked-examples/w3-distinct-subscriptions-same-plan-sum-not-coalesced", "no view value produced (unsupported view/watch path)"),
    ("w-worked-examples/w3-lifetime-pool-funds-spend-after-finite-expired", "no view value produced (unsupported view/watch path)"),
    ("w-worked-examples/w3-plan-credit-increase-affects-future-not-recorded-funding", "no view value produced (unsupported view/watch path)"),
    ("w-worked-examples/w3-spend-exceeding-all-pools-rejected", "no view value produced (unsupported view/watch path)"),
    ("w-worked-examples/w3-unfunded-account-cannot-draw-other-accounts-pools", "no view value produced (unsupported view/watch path)"),
    // --- fail:outcome ---
    ("05-state-model/bulk-insert-defaults-see-prestatement-state", "outcome divergence: expected `ok` observed `rejected`"),
    ("05-state-model/nested-initializer-failure-rejects-parent-insert", "outcome divergence: expected `ok` observed `rejected`"),
    ("05-state-model/rekey-constraint-failure-rejects-transition", "outcome divergence: expected `rejected` observed `ok`"),
    ("06-expressions/row-mutation-receiver-duplicate-occurrences-reject", "outcome divergence: expected `ok` observed `rejected`"),
    ("08-mutations-validation/internal-call-failure-rejects-caller-writes", "outcome divergence: expected `rejected` observed `ok`"),
    ("08-mutations-validation/replacement-restrict-ref-rejects-transition", "outcome divergence: expected `rejected` observed `ok`"),
    ("10-interfaces-roles/deleted-scope-row-revokes-role", "outcome divergence: expected `ok` observed `denied`"),
    ("10-interfaces-roles/fixed-call-argument-not-overridable", "outcome divergence: expected `ok` observed `denied`"),
    ("10-interfaces-roles/membership-reevaluated-each-admission", "outcome divergence: expected `ok` observed `rejected`"),
    ("10-interfaces-roles/recursive-descendant-mutation-addressing-unspecified", "outcome divergence: expected `ok` observed `denied`"),
    ("10-interfaces-roles/row-mutation-receiver-exactly-one", "outcome divergence: expected `rejected` observed `denied`"),
    ("10-interfaces-roles/scoped-role-addressed-by-row-and-name", "outcome divergence: expected `ok` observed `denied`"),
    ("11-auth-sessions/login-operation-id-replay-at-most-once", "outcome divergence: expected `ok` observed `rejected`"),
    ("11-auth-sessions/login-operation-id-reuse-different-request-rejected", "outcome divergence: expected `ok` observed `rejected`"),
    ("11-auth-sessions/login-token-immediately-usable", "outcome divergence: expected `ok` observed `rejected`"),
    ("12-clients-live-views/authority-loss-emits-close", "outcome divergence: expected `ok` observed `denied`"),
    ("12-clients-live-views/parameter-normalization-and-checks", "outcome divergence: expected `ok` observed `denied`"),
    ("12-clients-live-views/resume-after-authority-loss-denied", "outcome divergence: expected `ok` observed `denied`"),
    ("12-clients-live-views/revoked-member-call-denied-at-admission", "outcome divergence: expected `ok` observed `denied`"),
    ("14-buckets/between-rejects-empty-or-reversed-range", "outcome divergence: expected `rejected` observed `ok`"),
    ("15-meters/accrual-unspent-capacity-does-not-roll-over", "outcome divergence: expected `ok` observed `rejected`"),
    ("15-meters/backdated-spend-consumes-expired-pool", "outcome divergence: expected `ok` observed `rejected`"),
    ("15-meters/expired-pool-does-not-fund-current-spend", "outcome divergence: expected `ok` observed `rejected`"),
    ("15-meters/hierarchical-level-without-meter-adds-no-constraint", "outcome divergence: expected `ok` observed `rejected`"),
    ("15-meters/hierarchical-limits-clear-every-level", "outcome divergence: expected `ok` observed `rejected`"),
    ("15-meters/inactive-bucketed-spend-retains-allocation", "outcome divergence: expected `ok` observed `rejected`"),
    ("15-meters/plan-downgrade-preserves-recorded-funding", "outcome divergence: expected `ok` observed `rejected`"),
    ("15-meters/pool-removal-preserves-recorded-funding", "outcome divergence: expected `ok` observed `rejected`"),
    ("15-meters/recurring-periods-accrue-fresh-capacity", "outcome divergence: expected `ok` observed `rejected`"),
    ("15-meters/spend-at-pool-until-boundary-excluded", "outcome divergence: expected `ok` observed `rejected`"),
    ("15-meters/spend-update-failure-preserves-prior-allocation", "outcome divergence: expected `ok` observed `rejected`"),
    ("19-history-artifacts/unrelated-import-requires-policy", "outcome divergence: expected `ok` observed `rejected`"),
    ("21-deletion-erasure/collection-replacement-restrict-ref-rejects", "outcome divergence: expected `rejected` observed `ok`"),
    ("21-deletion-erasure/delete-grant-is-not-erasure-grant", "outcome divergence: expected `ok` observed `denied`"),
    ("22-runtime-semantics/actor-bound-for-authenticated-request", "outcome divergence: expected `ok` observed `denied`"),
    ("22-runtime-semantics/confusable-actor-names-provenance-stays-distinct", "outcome divergence: expected `ok` observed `denied`"),
    ("23-host-contract/restart-preserves-identity-values-and-view", "outcome divergence: expected `ok` observed `denied`"),
    // --- fail:valdiff ---
    ("06-expressions/composite-selector-member-order-irrelevant", "value diverges from expectation"),
    ("06-expressions/empty-text-key-addressable", "value diverges from expectation"),
    ("06-expressions/unicode-normalization-distinct-text-keys", "value diverges from expectation"),
    ("08-mutations-validation/replacement-validates-complete-collection", "value diverges from expectation"),
    // --- fail:viewdiff ---
    ("05-state-model/rekey-does-not-run-on-delete", "view result diverges from expectation"),
    ("05-state-model/rekey-updates-inbound-refs", "view result diverges from expectation"),
    ("05-state-model/row-check-constrains-complete-row", "view result diverges from expectation"),
    ("05-state-model/set-of-enum-reads-in-declaration-order", "view result diverges from expectation"),
    ("06-expressions/rekey-redirects-ref-key-equality", "view result diverges from expectation"),
    ("08-mutations-validation/rekey-rewrites-inbound-refs-atomically", "view result diverges from expectation"),
    ("09-loading-bootstrap/seed-default-observes-prospective-state", "view result diverges from expectation"),
    ("09-loading-bootstrap/seed-percent-encoded-key-round-trip", "view result diverges from expectation"),
    ("12-clients-live-views/temporal-observation-advances-live-view", "view result diverges from expectation"),
    ("12-clients-live-views/window-anchor-survives-rekey", "view result diverges from expectation"),
    ("14-buckets/expiration-preserves-row-in-all", "view result diverges from expectation"),
    ("18-blobs/claimed-sha512-mismatch-rejected", "view result diverges from expectation"),
    ("18-blobs/no-writable-store-rejects-upload", "view result diverges from expectation"),
    ("18-blobs/oversize-upload-rejected", "view result diverges from expectation"),
    ("18-blobs/replay-operation-id-upload-at-most-once", "view result diverges from expectation"),
    ("21-deletion-erasure/erased-row-unobservable-in-second-view", "view result diverges from expectation"),
    ("21-deletion-erasure/rekey-target-does-not-run-on-delete-cascade", "view result diverges from expectation"),
    ("22-runtime-semantics/concurrent-appends-either-order-both-atomic", "view result diverges from expectation"),
    ("22-runtime-semantics/cross-connection-sequential-order-unspecified", "view result diverges from expectation"),
    ("annex-b-total-order/composite-key-lexicographic-in-key-order", "view result diverges from expectation"),
    ("annex-b-total-order/composite-key-second-component-uses-int-order", "view result diverges from expectation"),
    ("annex-b-total-order/descending-reverses-with-none-first", "view result diverges from expectation"),
    ("annex-b-total-order/int-mathematical-order-with-negatives", "view result diverges from expectation"),
    ("annex-b-total-order/mixed-direction-keys-apply-per-key-direction", "view result diverges from expectation"),
    ("annex-b-total-order/optional-none-sorts-last-ascending", "view result diverges from expectation"),
    ("annex-b-total-order/secondary-sort-key-breaks-ties", "view result diverges from expectation"),
    ("annex-b-total-order/struct-fields-compared-in-field-name-text-order", "view result diverges from expectation"),
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