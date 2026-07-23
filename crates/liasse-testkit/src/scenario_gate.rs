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
    // --- `authenticate` step (CLOSED) ---
    // The `authenticate` step drives through `SurfaceHost::authenticate`, binding a
    // named multiplexed `context` (§11.8) via its `as` label, and a `watch`/`call`
    // naming that `context` threads it onto the surface subscription/call
    // (`with_context`, adapter/runtime.rs). So the §11.8 completion-barrier and
    // cross-context-isolation cases (`completion-barrier-spans-sessions`,
    // `multiple-credentials-one-connection`, `shared-connection-cross-context-isolation`)
    // all pass — no residual authenticate-step debt remains.
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
    // that read a placement member in a mutation `return` therefore pass. The
    // §18.7-step-4 descriptor binding is CLOSED: `stage` builds the client-declared
    // descriptor (honest content carrying the declared `$name`, or an explicit
    // `claim`'s members with the canonical Annex A.1 string-form `$bytes`, #20) and
    // binds the verified descriptor into the mutation call, so `.file.$name` lands in
    // committed state and a verifying `claim` commits — un-skipping
    // `same-content-different-metadata-distinct-descriptors`, `descriptor-bytes-encoding`,
    // and `descriptor-metadata-readable-in-view`.
    // §18.3 pins eager connector resolution: a store-row write feeding a declared
    // placement is rejected at admission when its connector is unregistered. This is
    // a genuine subsystem-crossing feature, not a harness gap, spanning THREE layers
    // the runtime does not currently reach: (1) liasse-model discards `$blob_storage`
    // after syntactic validation (`blob::check_all`) — the built model carries no
    // placement policy, so the runtime cannot know which store collections are
    // placement-reachable; (2) the runtime `Engine` holds no blob-connector registry
    // (connectors live in the driver's composed `BlobHost` and in `BlobEngine`, not
    // `Engine::call`'s admission); (3) no admission hook inspects a written store row
    // against a placement target. Absent all three, the store-row insert commits as
    // ordinary data instead of rejecting. Scoped as acknowledged debt against the
    // now-pinned §18.3 outcome (rejecting it in the harness alone would fake the
    // runtime property SPEC.md makes an admission rule, so it is left to the feature).
    ("18-blobs/connector-resolution-timing", "eager store-row connector validation (§18.3) is a subsystem-crossing feature: the model discards `$blob_storage` (no runtime placement-reachability), the engine holds no connector registry, and `Engine::call` admission has no store-row/placement hook — none surgical"),
    // --- `budget_set` step (CLOSED) ---
    // §23.6 makes the in-flight external-component bound MANDATORY and UNCONDITIONAL
    // ("a runtime MUST bound each in-flight external-component call … regardless of
    // whether a numeric budget is declared"); a non-returning call is a §17.9 failure
    // that rejects the enclosing request, committing no effect. The sim provider's
    // `hang` script models exactly that (`ProviderFailure::WouldNotReturn`), so the
    // runtime already produces the terminal §23.6 rejection through the signing path,
    // and its §22.2 atomicity leaves no partial row. `budget_set` is now driven
    // (adapter/ops.rs `drive_budget_set`: it establishes the budget precondition and
    // validates the declaration; the mandatory in-flight bound needs no separate
    // numeric gate), so `budget-exhaustion-rejects-not-backpressure` and
    // `budget-exhaustion-never-partial-transition` both pass and were removed here.
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
    // §19.5 `coverage` (included range + restorability) is now emitted by the
    // exporter and carried/parsed by the closed manifest, and §19.5/§19.7 manifest-
    // vs-history-index selection coherence is now verified at restore (those two
    // cases were pruned). The residual coherence case below is the STATE-section
    // splice, which needs the state section to carry its own Annex D.5 selection.
    ("19-history-artifacts/spliced-state-selection-mismatch-invalid","§19.7 state-vs-selected-point coherence is not verified at restore (the copy_entry_from splice is accepted): the portable state section carries no Annex D.5 header (its own `selected`/`definition`), so a spliced state entry has no embedded selection to disagree with the manifest — needs the D.5 state-section header, a §19.6/Annex D.5 format seam, not a surgical check"),
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
    //
    // Single-instance interface addressing (`.modules[key]::iface`, §13.4/§13.9/
    // §13.10/W4) now type-checks and loads: the `::` traversal checker accepts a
    // single selected-instance row base as well as a whole-space view base
    // (liasse-expr `check_traverse`/`traverse_view`), so every §13 and w4 ROOT
    // package now compiles in a fresh engine. The residual per entry below is the
    // module-runtime feature the deployment still lacks (peer/`$parent` import
    // resolution at child compile/genesis, cross-module dispatch, contract checks,
    // update recheck/report), NOT the surface-address compile that used to block.
    ("13-modules/cross-module-atomic-transition", "§13.10 the interface-addressed `$public` surfaces now compile and the two children install; the residual is cross-module atomic mutation dispatch (the `::iface.mut` interface call at step 4 is `rejected`) — the owner-side metered transition and the caller state change in one atomic cross-module commit are unlanded"),
    ("13-modules/uninstall-blocked-by-cross-boundary-restrict-ref", "§13.12 the interface-addressed `$public` surfaces now compile; the residual is cross-boundary `$on_delete: restrict` — installing the second child that refs across the boundary is `rejected` (its cross-boundary ref binding at install is unlanded), before uninstall-blocking can be exercised"),
    // (§13.8/§13.10 interface-contract satisfaction now runs at install: the module
    // host checks each declared interface `$mut` contract against the private
    // mutation the child binds — the bound mutation may read only the parameters the
    // interface prototype supplies (parameter contract) and MUST project every
    // `$return` field with the declared type (response contract), rejecting a
    // mismatch as `ModuleError::InterfaceContract` → `invalid`. This landed
    // `expose-binding-contract-mismatch-invalid` (response direction) and
    // `interface-mutation-param-contract-mismatch-invalid` (parameter direction);
    // their entries were pruned as stale.)
    // (§13.3 installation `$data` overlay now performs the full three-way merge onto
    // an already-seeded row — `SeedMode::Overlay` in `seed.rs`: writable scalar/struct
    // fields replace, `$set` fields union, omitted fields are retained, and nested
    // keyed child collections merge by key. `install-data-overlay-merge` passes and
    // its ledger entry was pruned as stale.)
    // Private `$deps` provisioning (§13.6): the consumer child (`t.checkout`) declares
    // `$deps: { tax: "t.tax@1" }` and its `$expose` addresses `#tax.rates` / `#tax.rates.set`.
    // The install returns `invalid` because the consumer fails a standalone compile: `$deps`
    // handles are validated (`liasse-model` `check_deps`) but never registered as import
    // bindings, so `#tax` is an unknown `#import` (liasse-expr `env::import`/`scope::import`),
    // AND the `#tax.rates` shape is a NEW import kind — a module-INSTANCE handle whose
    // `.interface` field selects the dep's exposed-interface collection and whose
    // `.interface.mut` names an exposed mutation — distinct from the §13.5 peer binding
    // (`ModuleHost::bind_peer`), which binds `#handle` directly to ONE interface collection.
    // Closing it needs: (1) a package-line resolver so `ModuleHost::install` can turn a
    // `DepSpec { line, major }` into the dep definition (the adapter holds the packages map,
    // the host holds none); (2) per-consumer nested dep-engine provisioning owned by the
    // child (isolation is then structural, as for siblings); (3) the module-instance import
    // type + `#tax.rates(.set)` typing/eval in liasse-expr; (4) dep-routed interface-mutation
    // dispatch (`ModuleHost::interface_call` routing `taxes.set` → `#tax.rates.set` → the dep's
    // exposed `rates.set` → `.set_rate`). A multi-crate §13.6 feature, not a surgical fix.
    ("13-modules/private-deps-isolated-per-consumer", "§13.6 `$deps` private nested-instance provisioning is unlanded: the consumer fails standalone compile because `$deps` handles are never registered as `#import` bindings and the `#tax.rates` module-instance import kind + per-consumer dep-engine provisioning + dep-routed mutation dispatch do not exist (multi-crate: liasse-model/liasse-expr/liasse-runtime)"),
    // §13.6 sibling privacy would itself fall out once deps are provisioned (a private dep is
    // not in `ModuleHost::siblings`, so `spy`'s required peer `t.tax/rates@1` already resolves
    // to zero candidates → `PeerUnresolved` → `rejected`); but step 0 installs `t.checkout`,
    // which requires the same unlanded `$deps` provisioning, so the case never reaches step 1.
    ("13-modules/sibling-cannot-address-private-dep-rejected", "§13.6 `$deps` provisioning is unlanded (see private-deps-isolated-per-consumer); step 0 installing the consumer `t.checkout` fails standalone compile, so the sibling-privacy rejection at step 1 is never exercised (the privacy check itself already holds: a private dep is not a `ModuleHost::siblings` candidate)"),
    // (§13.7 `$if_module`-guarded declarations now land: the model grammar accepts
    // `$if_module` on a `$model` collection and an `$expose` interface, the child
    // compiles, and the composition host gates a guarded interface's boundary
    // occurrence on its optional `$use` handle being bound to an enabled instance —
    // disabling the peer withdraws the guarded exposure and re-enabling restores the
    // preserved rows (§13.7/§13.12). The child's `add` inline `$expose` `$mut` program
    // dispatches as a synthetic root mutation, so `if-module-guarded-state-preserved`
    // passes and its entry was pruned as stale.)
    // Update path (§13.14/§13.15): the exposed-surface narrowing recheck and the
    // §13.15 update-report assembly are landed (modules/compat.rs + host.rs), so
    // `minor-update-narrowing-rejected`, `update-narrowing-view-field-rejected`, and
    // `update-result-report` now pass and are off the ledger.
    // (§4.1/§13.13 `$bundle` now lands: the model accepts `$bundle` and validates it
    // as insert data disjoint from `$seed`; genesis applies it as ordinary inserts;
    // and update runs the three-way merge among old bundle, new bundle, and migrated
    // state (insert-new / replace-if-current-equals-old / retain-local-on-removal).
    // Landing it also closed a §8.3 model gap — a param assigned to a local-binding
    // field (`t.label = @label`) is now inferred — and its runtime analogue — a local
    // bound to a keyed row is a live write target. `update-bundle-three-way-merge`
    // passes and its entry was pruned as stale.)
    // --- §13 module lifecycle used by other chapters (same seams) ---
    ("19-history-artifacts/child-export-matches-embedded-artifact", "§19 child-module `.liasse` artifact export/embedding is unlanded; the case also mounts a non-absolute module space `mods` the runtime `ModuleSpace` rejects"),
    ("19-history-artifacts/child-module-artifact-embedded-and-extractable", "§19 child-module `.liasse` artifact export/embedding is unlanded; the case also mounts a non-absolute module space `mods` the runtime `ModuleSpace` rejects"),
    ("19-history-artifacts/tampered-child-artifact-invalid", "§19 child-module `.liasse` artifact embedding/verification is unlanded; the case also mounts a non-absolute module space `mods` the runtime `ModuleSpace` rejects"),
    ("w-worked-examples/w4-host-imports-exposed-template-across-boundary", "§13.9 the §13.4 parent surface now resolves so the child installs and its exposed template aggregates; the residual is module-aware root-MUTATION admission — the host `import_template` mutation reads `.modules[@module]::templates[@template]` inside its program, but a root mutation admits with no module aggregate (only root VIEW reads fold it), and the adapter routes the plain root-mutation call and its `.templates` read to the base host (no children) rather than the deployment, so `source` selects zero and the insert rejects (§6.3)"),
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
    // (`10-interfaces-roles/duplicate-membership-no-extra-authority` now passes: its
    // top-level role `$members: ".groups[:g].members[:m].account"` is a nested
    // flatten ending in a `.field` actor-key projection (§10.3). The auth-plan
    // reconstruction (adapter/auth.rs `plan_role`) now projects that trailing field
    // as a NAMED output (`{ account }`) — mirroring the scoped-role path — instead of
    // navigating into a view over a scalar (which the checker rejects), so the
    // synthetic membership view compiles and the case runs. Entry pruned as stale.)
    // 11-auth session/host-verifier wiring is live (adapter/auth.rs); the
    // `committed-request-final-after-revocation` residual needs a seam the auth
    // wiring does not reach: a scoped-role inline `$mut`
    // (`/sessions[$session.$key].revoke()`) reading the request-scoped `$session`,
    // which the surface router does not bind. (The bucket-expiry reconstruction is
    // now derived from the collection's `$bucket` `$until`, so a session with an
    // explicit `$from` lower bound activates at its boundary —
    // `session-not-yet-active-denied` passes and was pruned here.)
    ("11-auth-sessions/committed-request-final-after-revocation", "scoped-role session `revoke()` mutation not bound (denied)"),
    // §14.5 bounded temporal read of an unbounded recurring source-backed bucket
    // now generates the series to the selector's own bound, so `.$at`/`.$between`
    // past the clock resolve; the rollover-at-boundary, future-spanning window, and
    // calendar-monthly-clamp cases pass and were pruned from the ledger.
    ("14-buckets/dst-fall-back-ambiguous-earlier", "§14.7 seed rejects: the source-backed bucket's calendar period names IANA zone `Europe/Paris`, which `jiff` cannot resolve — the build configures no time-zone database (`liasse-value` pins jiff `default-features=false`); AND the §14.7 DST `ambiguous`/`missing` resolution policy this case tests is itself unlanded (recur.rs marks those branches unreachable). Needs a deterministic bundled tzdb PLUS the §14.7 ambiguous-policy implementation — a §14.7 feature, not a surgical fix"),
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
    // (§16.5 verifier-namespace-runs-at-admission now PASSES: the testkit carries a
    // registered verifier's `accepts` table onto the registered-namespace dispatch
    // path used inside a mutation body (adapter/namespaces.rs seeds the sim
    // namespace's `accept` table) and wires a `$verify: "$credential"` + whole-proof
    // `$session` literal-session authenticator (adapter/auth.rs), so `authns.check`
    // executes in the login mutation and the minted session id authenticates. Entry
    // pruned as stale.)
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
    // (`18-blobs/descriptor-metadata-readable-in-view` now passes: view-shape corpus
    // error fixed to the §6.3/§12.2 one-row array, and the §18.7-step-4 descriptor
    // binding now carries the declared `$name`, so the projected `name: .file.$name`
    // member is present in the row. Entry pruned as stale.)
    // (`18-blobs/surplus-copy-after-policy-shrinks` now passes: the `set_enabled`
    // mutation's bound-patch form (`s = .stores[@id] { … }`) is not a spec-defined
    // binding — §8.4 enumerates insert/insert-from-view/replace/delete results and a
    // patch is a statement per §8.6/Annex C.9 — so the corpus case was rewritten to
    // the §8.10 direct form (`.stores[@id] { … }` then `return .stores[@id] { … }`),
    // yielding the same `{ id, enabled }` row. Entry pruned as stale.)
    // (`annex-d-identity/ref-wire-value-is-current-typed-key` now passes: the case
    // was mis-authored — it read `p.author.name`, a bare `ref.field` access, which
    // §7.6 does not define (a ref value is a target KEY, A.9; dereference uses the
    // normal SELECTOR, §7.6 `/accounts[.owner]`). Corrected to the selector form
    // `/users[p.author].name` (the same §7.6 deref `07-views/
    // ref-dereference-yields-target-row` exercises); the §D.1 wire-value-tracks-
    // current-key-across-rekey intent and every asserted value are unchanged. Entry
    // pruned as CORPUS-FIX.)
    // (W2 auth cluster now PASSES end to end. The testkit executes the simulated
    // `$requires` host namespaces INSIDE the §11.5 login mutation body (§16.5):
    // adapter/authsim.rs synthesizes executable namespaces for the case's declared
    // response/token tables (`webauthn.verify` → the responses lookup, `token.sign` →
    // a self-describing minted token) and `Engine::load_with_dispatch` (lenient
    // checker + live registry) dispatches them; adapter/auth.rs then decodes that
    // self-describing token in the `token.verify` `$verify` seam. Landing the login
    // additionally required closing five core-language gaps the flow is the first to
    // exercise (all spec-correct, all shared by the now-passing `login-*` cases):
    // struct-field read on a host result (`identity.rp`, §5.8), composite-key→`ref`
    // key coercion + identity-form key comparison (`login.$key` into a `ref` key
    // component, §6.3/§D.1), the `time.duration` core builtin and `timestamp +
    // duration` arithmetic (§16.1/§11.5), and `$from`/`$until` interval structurals
    // on a lifecycle-bucketed row (`session.$until`, §14.4). All nine w2 cases were
    // pruned from this ledger as stale.)
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
    // (§14.1/§22.6 `14-buckets/short-form-from-defaults-to-created` now PASSES: the
    // ledger reason ("no view value produced") was STALE — the watch WAS driven; its
    // value was wrong. A short-form `$bucket: ".expires_at"` defaults `$from` to
    // `$created`, the row's RECORDED admission instant, which the runtime never
    // stored — so a back-dated `.$at(t)` before the row's creation wrongly reported it
    // active. The store now records `now()` per committed row (`StoredRow::created`,
    // `CommittedTransition::created`) in BOTH backends and the materializer binds it as
    // the `$from` cell, so `.$at(T0-1us)` correctly excludes a row admitted at T0.
    // Entry removed as passing.)
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
    // (§11.5 `login-token-immediately-usable`, `login-operation-id-replay-at-most-once`,
    // and `login-operation-id-reuse-different-request-rejected` now PASS: their login
    // mutations call `token.sign` in the body, which the testkit now executes
    // (adapter/authsim.rs) and whose minted token the auth layer decodes — the same
    // §16.5 host-execution seam the w2 cluster uses. Entries pruned as stale.)
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
