//! Corpus conformance: drive every `suite: static` case from the file-based
//! corpus (`tests/`, see `tests/FORMAT.md`) through [`Model::build`] and assert
//! the model's accept/reject decision matches the case's expected load outcome.
//!
//! This is the bridge between the implementation-agnostic corpus and the CORE
//! static model. A case's `package` (raw JSON) is serialized to strict-JSON text
//! — a subset of the authoring form `liasse_syntax::parse_document` accepts —
//! parsed to a spanned document, and built. The model yields exactly two
//! decisions, mapped to the FORMAT.md outcome vocabulary:
//!
//! - a constructed [`Model`] ⇒ `ok` (the package loaded);
//! - error diagnostics ⇒ the package did not load. For the *static* suite this
//!   satisfies both `invalid` (statically rejected) and `rejected` (the §9
//!   load-time seed admission checks the model performs at build time — seed
//!   rows pass through the same ref/key/check/uniqueness rules as inserts).
//!
//! `outcome: unspecified` cases are recorded as skipped, never judged (FORMAT.md
//! records them without a verdict). Cases whose expected outcome genuinely needs
//! machinery from a later phase (module composition, host/runtime admission that
//! the model cannot see statically) are listed in [`SKIP`] with a one-line
//! reason each; the list is expected to shrink as later phases land. Scenario
//! cases are out of scope — there is no runtime yet.

// Tests panic on a failed assertion (AGENTS.md), which the workspace deny-lints
// otherwise forbid.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use liasse_diag::SourceMap;
use liasse_model::Model;
use liasse_syntax::parse_document;
use liasse_testkit::{Corpus, LoadedCase, Outcome, PackageSet, Suite};

/// Cases whose expected static outcome cannot be decided by the CORE
/// single-package model and that legitimately await a later phase. Each entry is
/// `"<area>/<name>"` with a one-line reason. This list must only shrink.
const SKIP: &[(&str, &str)] = &[
    // --- §13 module composition (cross-package) ---
    // The invalid outcome is a cross-package exposure rule (a child's private
    // path/field is not reachable from the host root). Judging it needs
    // multi-package composition, absent from the single-package CORE model.
    (
        "13-modules/private-child-field-not-exposed-invalid",
        "cross-package module composition (§13); needs the composition phase",
    ),
    (
        "13-modules/direct-child-private-path-call-invalid",
        "cross-package module composition (§13); needs the composition phase",
    ),
    // --- Surface-inline mutation programs (documented surface seam) ---
    // A program written directly in a surface `$mut` (not a `.name` reference) is
    // accepted structurally today (crates/liasse-model/src/surface.rs). Checking
    // its write target / actor scope needs it routed through the mutation phase.
    (
        "05-state-model/write-to-computed-value-invalid",
        "write-target check inside a surface-inline mutation program (§5.2); surface seam",
    ),
    (
        "10-interfaces-roles/public-surface-cannot-bind-actor",
        "$actor-scope check inside a surface-inline mutation program (§10/§11); surface seam",
    ),
    // --- §16 host-namespace / §9.2 provider+namespace resolution ---
    // The model's lib.rs documents host-namespace resolution as a CORE seam: the
    // expr registry carries only the attested core functions, and no host
    // context resolves declared/aliased namespaces or their versions.
    (
        "04-package-structure/missing-required-namespace-rejected",
        "required-namespace resolution (§16.2/§9.2); host-namespace seam",
    ),
    (
        "09-loading-bootstrap/missing-required-namespace-rejects-load",
        "required-namespace resolution at load (§16.2/§9.2); host-namespace seam",
    ),
    (
        "09-loading-bootstrap/incompatible-namespace-major-fails-before-activation",
        "namespace version-compatibility resolution (§16/Annex E); host-namespace seam",
    ),
    (
        "16-host-namespaces/compatible-minor-resolves-within-major",
        "host expr-namespace version resolution (§16); registry lacks `util.*`",
    ),
    (
        "16-host-namespaces/core-namespaces-load-without-requires",
        "core namespace expr functions beyond the attested subset not in the expr registry (§16.1)",
    ),
    (
        "16-host-namespaces/requires-key-aliases-expression-namespace",
        "aliased host expr-namespace resolution (§16.2); registry lacks `codec.*`",
    ),
    // --- §17 keyring provider capabilities / §9.2 provider registry ---
    // Each needs a resolved host key provider and its declared capabilities,
    // which the CORE model has no host context to consult.
    (
        "17-keyrings/keyring-unregistered-provider-invalid",
        "host key-provider registry resolution (§9.2/§17.6); provider seam",
    ),
    (
        "17-keyrings/automatic-rotation-requires-generation-capability-invalid",
        "provider generation-capability check (§17.6); provider seam",
    ),
    (
        "17-keyrings/manual-policy-requires-binding-capability-invalid",
        "provider binding-capability check (§17.6); provider seam",
    ),
    (
        "17-keyrings/protection-class-unmet-invalid",
        "provider protection-class capability check (§17); provider seam",
    ),
    (
        "17-keyrings/provider-lacks-algorithm-capability-invalid",
        "provider algorithm-capability check (§17); provider seam",
    ),
    (
        "17-keyrings/provider-name-unicode-confusable-invalid",
        "provider-name identity/registry resolution (§17/Annex D); provider seam",
    ),
    (
        "17-keyrings/seed-keyring-version-invalid",
        "keyring-managed version seeding admission (§17/§9); provider seam",
    ),
    // --- §4 resource descriptors (need the built artifact) ---
    (
        "09-loading-bootstrap/resource-digest-mismatch-rejected",
        "resource $sha256 verification over artifact entries (§4.1); needs the artifact",
    ),
    (
        "09-loading-bootstrap/resource-path-identifies-no-entry-rejected",
        "resource $path resolution over artifact entries (§4.1); needs the artifact",
    ),
    // --- §9 seed admission over materialized prospective state ---
    // The model type-checks $data, but ref/unique/$check/repeated-key admission
    // needs the seed rows materialized into a prospective state and run through
    // the load-admission pipeline (a runtime concern).
    (
        "09-loading-bootstrap/seed-ref-must-resolve",
        "ref admission over seeded rows (§9.1/§5.6); load-admission seam",
    ),
    (
        "09-loading-bootstrap/seed-unique-violation-rejects-load",
        "$unique admission over seeded rows (§9.1); load-admission seam",
    ),
    (
        "09-loading-bootstrap/seed-check-violation-rejects-load",
        "$check admission over seeded rows (§9.1); load-admission seam",
    ),
    (
        "09-loading-bootstrap/seed-repeated-key-field-must-agree",
        "composite-key agreement admission over seeded rows (§9.1); load-admission seam",
    ),
    (
        "09-loading-bootstrap/seed-noncanonical-key-alias-rejected",
        "Annex D canonical key-text check on seed keys (§9/Annex D); load-admission seam",
    ),
    // --- §6/§7 identity-domain typing (needs expr-layer domains) ---
    (
        "06-expressions/cross-relation-ref-comparison-invalid",
        "ref identity-domain typing for `==` (§6); needs expr-layer ref domains",
    ),
    (
        "07-views/combinator-mismatched-identity-domain-invalid",
        "view-combinator identity-domain agreement (§7); needs expr-layer view domains",
    ),
    // --- §10/§11 role/actor typing ---
    (
        "10-interfaces-roles/members-actor-type-mismatch-invalid",
        "$members-vs-authenticator $actor row-type agreement (§10/§11); $actor typing is a later pass",
    ),
    // --- §23 host contract ---
    (
        "23-host-contract/unavailable-component-fails-before-activation",
        "host component-availability resolution (§23/§9.2); host seam",
    ),
    // --- §18 blob-connector registry (host context) ---
    // The expected rejection is "placement selects a store whose connector is
    // not registered" against the case's `hosts.connectors` context, which this
    // bridge never reads and the CORE model has no host registry to consult.
    // The case previously *appeared* to pass only because its `return doc { id }`
    // statement hit the since-fixed `return <name>` grammar bug and the package
    // was rejected for the wrong reason.
    (
        "18-blobs/unregistered-connector-fails-load",
        "host blob-connector registry resolution (§18.12/§2.1); host seam",
    ),
];

/// The model's binary decision for a static case.
enum Decision {
    /// The package built into a [`Model`].
    Loaded,
    /// The package was rejected; carries the rendered diagnostics for debugging.
    Rejected(String),
}

/// Serialize the raw package JSON, parse it, and build the model.
fn build_package(package: &serde_json::Value) -> Decision {
    let text = serde_json::to_string_pretty(package).expect("package value serializes to JSON");
    let mut sources = SourceMap::new();
    let id = sources.add_file("package.liasse", &text);
    match parse_document(id, &text) {
        Err(diags) => Decision::Rejected(diags.render(&sources)),
        Ok(document) => match Model::build(&mut sources, id, &document) {
            Ok(_) => Decision::Loaded,
            Err(diags) => Decision::Rejected(diags.render(&sources)),
        },
    }
}

/// Whether a rejecting model satisfies this expected static outcome. `ok`
/// requires a load; `invalid`/`rejected` (load did not succeed) are satisfied by
/// a rejection. `unspecified` is filtered out before this is called.
fn reject_satisfies(expected: Outcome) -> bool {
    matches!(expected, Outcome::Invalid | Outcome::Rejected)
}

/// A single case's judged result.
enum Verdict {
    Pass,
    Skipped,
    /// `(expected token, rendered detail)`.
    Fail(String, String),
}

fn judge(case: &LoadedCase) -> Verdict {
    if case.case.suite != Suite::Static {
        return Verdict::Skipped;
    }
    let key = format!("{}/{}", case.area.as_str(), case.case.name);
    if SKIP.iter().any(|(k, _)| *k == key) {
        return Verdict::Skipped;
    }
    let expect = match &case.case.body {
        liasse_testkit::CaseBody::Static(expect) => expect,
        liasse_testkit::CaseBody::Scenario(_) => return Verdict::Skipped,
    };
    // An `expect` with no explicit outcome is an implicit `ok`.
    let expected = expect.outcome.unwrap_or(Outcome::Ok);
    if expected == Outcome::Unspecified {
        return Verdict::Skipped;
    }
    // Multi-package (module/migration) cases need cross-package composition; if
    // one reaches here it is not skip-listed, so report it rather than guess.
    let package = match &case.case.packages {
        PackageSet::Single(value) => value,
        PackageSet::Multi { packages, root } => {
            match root.as_ref().and_then(|r| packages.get(r)) {
                Some(value) => value,
                None => {
                    return Verdict::Fail(
                        expected.to_string(),
                        "multi-package case with no resolvable root; skip-list it".to_owned(),
                    );
                }
            }
        }
    };

    match (expected, build_package(package)) {
        (Outcome::Ok, Decision::Loaded) => Verdict::Pass,
        (Outcome::Ok, Decision::Rejected(diags)) => Verdict::Fail(
            "ok".to_owned(),
            format!("model rejected a package the case expects to load:\n{diags}"),
        ),
        (other, Decision::Rejected(diags)) if reject_satisfies(other) => {
            let _ = diags;
            Verdict::Pass
        }
        (other, Decision::Rejected(_)) => Verdict::Fail(
            other.to_string(),
            format!("expected outcome `{other}` is not a static reject class"),
        ),
        (other, Decision::Loaded) => Verdict::Fail(
            other.to_string(),
            format!("model built a package the case expects to be `{other}`"),
        ),
    }
}

#[test]
fn corpus_static_cases_match_expected_outcome() {
    let corpus = Corpus::load().expect("corpus loads");

    let mut passing = 0usize;
    let mut skipped = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for case in &corpus.cases {
        match judge(case) {
            Verdict::Pass => passing += 1,
            Verdict::Skipped => skipped += 1,
            Verdict::Fail(expected, detail) => {
                failures.push(format!(
                    "\n==== {} ====\n  path:     {}\n  expected: {}\n  {}",
                    case.case.name,
                    case.path.display(),
                    expected,
                    detail,
                ));
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "{} of {} judged static cases failed ({} passing, {} skipped):\n{}",
            failures.len(),
            passing + failures.len(),
            passing,
            skipped,
            failures.join("\n"),
        );
    }
    println!("corpus static conformance: {passing} judged cases passing, {skipped} skipped");
}
