//! RED-TEAM finding (WAVE 3) — §17.2 / §7.1 / §7.2 / §7.3:
//! a keyring PUBLIC VIEW cannot be projected, sorted, or grouped.
//!
//! §17.2 exposes a keyring as a *view* of version-metadata rows and pins the
//! member names each version exposes:
//!
//!     id, algorithm, public_key, created_at,
//!     activated_at?, retired_at?, revoked_at?, attestation?
//!
//! §7.1 makes a view's projection the common read shape ("Views serve as the
//! common read abstraction for internal reuse, external APIs, role membership,
//! and meter sources."), §7.2 lets a projection declare a synthetic `$key` to
//! group, and §7.3 lets it declare `$sort`. Nothing in §17.2 or §7 exempts a
//! keyring public view (`.$current`/`.$accepted`/`.$public`/`.$versions`) from
//! ordinary projection: a package that wants to expose a JWKS-style key endpoint
//! (project only `id, public_key, created_at`; hide `attestation`), sort
//! `$accepted` by `created_at`, or group `$versions` by `algorithm` is doing
//! exactly what §7 defines a view to do over the rows §17.2 defines.
//!
//! But the model types a `$keyring` node's version-view row as an EMPTY keyless
//! row (`crates/liasse-model/src/build/shapes.rs:158-164`,
//! `RowType::keyless(std::iter::empty())`), on the stale rationale that
//! "§17.2 pins no version-metadata member names (SPEC-ISSUES 18)" — yet §17.2 now
//! lists them, and the corpus itself asserts `algorithm: "Ed25519"` on version
//! rows (`17-keyrings/common/automatic-bootstrap-activates-first-version`). With
//! zero declared fields, EVERY member reference in a projection/sort/group over a
//! keyring view is a static error:
//!   * a projection output name `algorithm` -> "unknown name `algorithm`"
//!     (`crates/liasse-expr/src/check/mod.rs::resolve_name`, no field on the row);
//!   * a `.$current.algorithm` field read -> "no field `algorithm` on this row"
//!     (`crates/liasse-expr/src/check/mod.rs:514`).
//!
//! So the whole package fails static validation and never loads.
//!
//! The three FINDINGs below each FAIL against the current implementation (the
//! package does not load, so the watch step is skipped). Every paired CONTROL —
//! the identical projection/sort/group over an ORDINARY collection carrying an
//! `algorithm` field, plus a pass-through keyring view — PASSES, isolating the
//! defect to the keyring-view empty-row-shape gap. Expected values are derived
//! from SPEC (§17.1/§17.2: a bootstrapped ring has exactly one active/accepted
//! version whose `algorithm` is the declared `Ed25519`; §17.3), never from
//! observed behaviour.
//!
//! DRY records (probed, no NEW divergence — see the module tail):
//!   * charge #2 (blob descriptor in a scoped-role recursive `$where`, §10.5):
//!     HOLDS — `child.report.$bytes` is readable in `$where` and authorization
//!     stays correct.
//!   * charge #3 (blob descriptor member aggregated inside a GROUPED view,
//!     `sum(group.file.$bytes)`): a KNOWN, already-documented static-rejection
//!     seam (`crates/liasse-testkit/src/scenario_gate.rs:308`, the §18.11
//!     `sum(...file.$bytes)` billing view), not a new finding. The
//!     projected-member form `sum(group { z: .file.$bytes }.z)` loads.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, SuiteKind};

fn run_case_text(text: &str) -> CaseResult {
    let case = Case::from_hjson(text, Path::new("<w3-keyring-blob-view>"), &BTreeSet::new())
        .expect("case parses");
    let mut adapter = liasse_testkit::ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("w3-keyring-blob-view"), SuiteKind::Red, &case)
}

fn assert_all_pass(result: &CaseResult, name: &str) {
    let ok = result.steps.iter().all(|s| s.result.is_pass());
    if !ok {
        for step in &result.steps {
            println!(
                "  {name} step {} [{}] -> {:?} observed={:?}",
                step.index, step.action, step.result, step.observed
            );
        }
    }
    assert!(ok, "{name}: a step diverged from its SPEC-derived expectation (see dump)");
}

// The keyring host wiring every keyring case shares: a registered provider that
// advertises the declared Ed25519 algorithm, automatic generation, and sign
// (§17.6). An automatic `$rotate` cadence bootstraps and activates version 1
// (§17.3), so `$public`/`$accepted`/`$versions` each expose that one version.
const KR_HOSTS: &str = r##"hosts: { key_providers: { "test-kp": {
        algorithms: ["Ed25519"], operations: ["sign"], generate: true, bind: false, protection: "software" } } }"##;

// ── FINDING 1 ────────────────────────────────────────────────────────────────
// §17.2 + §7.1: projecting a keyring public view down to the SPEC-exposed
// `algorithm` member is a valid view (each accepted version's `algorithm` is the
// declared `Ed25519`). It instead fails static validation ("unknown name
// `algorithm`") and the package never loads.
#[test]
fn keyring_public_view_projection_must_load() {
    let text = format!(
        r##"{{
      format: 1
      name: keyring-public-view-projection
      suite: scenario
      spec: ["#keyrings", "§17.2", "#views", "§7.1"]
      {KR_HOSTS}
      package: {{
        $liasse: 1
        $app: "t.krv.proj@1.0.0"
        $model: {{
          session_keys: {{ $keyring: {{ $provider: "test-kp", $algorithm: "Ed25519", $rotate: "P30D", $retain: "P45D" }} }}
          $public: {{ ring: {{ $view: "/session_keys.$public {{ algorithm }}" }} }}
        }}
      }}
      steps: [
        {{ watch: "public.ring", id: "w1",
          expect_init: {{ value: [ {{ algorithm: "Ed25519" }} ] }} }}
      ]
    }}"##
    );
    assert_all_pass(&run_case_text(&text), "keyring-public-view-projection");
}

// CONTROL 1a: the pass-through keyring public view (no projection) loads and
// exposes `algorithm` — the member IS exposed; only projecting it is blocked.
#[test]
fn control_keyring_public_view_passthrough_loads() {
    let text = format!(
        r##"{{
      format: 1
      name: control-keyring-public-passthrough
      suite: scenario
      spec: ["#keyrings", "§17.2"]
      {KR_HOSTS}
      package: {{
        $liasse: 1
        $app: "t.krv.pass@1.0.0"
        $model: {{
          session_keys: {{ $keyring: {{ $provider: "test-kp", $algorithm: "Ed25519", $rotate: "P30D", $retain: "P45D" }} }}
          $public: {{ ring: {{ $view: "/session_keys.$public" }} }}
        }}
      }}
      steps: [
        {{ watch: "public.ring", id: "w1",
          expect_init: {{ value: {{ $unordered: [ {{ algorithm: "Ed25519", "...": true }} ] }} }} }}
      ]
    }}"##
    );
    assert_all_pass(&run_case_text(&text), "control-keyring-public-passthrough");
}

// CONTROL 1b: the identical projection over an ORDINARY collection with an
// `algorithm` field loads and returns the projected row.
#[test]
fn control_ordinary_collection_projection_loads() {
    let text = r##"{
      format: 1
      name: control-ordinary-projection
      suite: scenario
      spec: ["#views", "§7.1"]
      package: {
        $liasse: 1
        $app: "t.krv.ord1@1.0.0"
        $model: {
          rings: { $key: "id", id: "text", algorithm: "text" }
          $public: { ring: { $view: ".rings { algorithm }" } }
        }
        $data: { rings: { r1: { algorithm: "Ed25519" } } }
      }
      steps: [
        { watch: "public.ring", id: "w1", expect_init: { value: [ { algorithm: "Ed25519" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-ordinary-projection");
}

// ── FINDING 2 ────────────────────────────────────────────────────────────────
// §17.2 + §7.3: sorting a keyring public view by an exposed member is a valid
// view. It instead fails static validation and never loads.
#[test]
fn keyring_public_view_sort_must_load() {
    let text = format!(
        r##"{{
      format: 1
      name: keyring-public-view-sort
      suite: scenario
      spec: ["#keyrings", "§17.2", "#views", "§7.3"]
      {KR_HOSTS}
      package: {{
        $liasse: 1
        $app: "t.krv.sort@1.0.0"
        $model: {{
          session_keys: {{ $keyring: {{ $provider: "test-kp", $algorithm: "Ed25519", $rotate: "P30D", $retain: "P45D" }} }}
          $public: {{ ring: {{ $view: "/session_keys.$accepted {{ algorithm, $sort: [algorithm] }}" }} }}
        }}
      }}
      steps: [
        {{ watch: "public.ring", id: "w1",
          expect_init: {{ value: [ {{ algorithm: "Ed25519" }} ] }} }}
      ]
    }}"##
    );
    assert_all_pass(&run_case_text(&text), "keyring-public-view-sort");
}

// CONTROL 2: the identical `$sort` over an ordinary collection loads.
#[test]
fn control_ordinary_collection_sort_loads() {
    let text = r##"{
      format: 1
      name: control-ordinary-sort
      suite: scenario
      spec: ["#views", "§7.3"]
      package: {
        $liasse: 1
        $app: "t.krv.ord2@1.0.0"
        $model: {
          rings: { $key: "id", id: "text", algorithm: "text" }
          $public: { ring: { $view: ".rings { algorithm, $sort: [algorithm] }" } }
        }
        $data: { rings: { r1: { algorithm: "Ed25519" } } }
      }
      steps: [
        { watch: "public.ring", id: "w1", expect_init: { value: [ { algorithm: "Ed25519" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-ordinary-sort");
}

// ── FINDING 3 ────────────────────────────────────────────────────────────────
// §17.2 + §7.2: grouping a keyring public view by an exposed member (synthetic
// `$key`) with an aggregate output is a valid view. A bootstrapped ring exposes
// exactly one accepted version (§17.3), so grouping `$versions` by `algorithm`
// yields one group with `count(group) == 1`. It instead fails static validation.
#[test]
fn keyring_public_view_group_must_load() {
    let text = format!(
        r##"{{
      format: 1
      name: keyring-public-view-group
      suite: scenario
      spec: ["#keyrings", "§17.2", "§17.3", "#views", "§7.2", "§7.5"]
      {KR_HOSTS}
      package: {{
        $liasse: 1
        $app: "t.krv.group@1.0.0"
        $model: {{
          session_keys: {{ $keyring: {{ $provider: "test-kp", $algorithm: "Ed25519", $rotate: "P30D", $retain: "P45D" }} }}
          $public: {{ ring: {{ $view: "/session_keys.$versions {{ $key: algorithm, algorithm, n: count(group) }}" }} }}
        }}
      }}
      steps: [
        {{ watch: "public.ring", id: "w1",
          expect_init: {{ value: [ {{ algorithm: "Ed25519", n: "1" }} ] }} }}
      ]
    }}"##
    );
    assert_all_pass(&run_case_text(&text), "keyring-public-view-group");
}

// CONTROL 3: the identical grouping over an ordinary collection loads.
#[test]
fn control_ordinary_collection_group_loads() {
    let text = r##"{
      format: 1
      name: control-ordinary-group
      suite: scenario
      spec: ["#views", "§7.2", "§7.5"]
      package: {
        $liasse: 1
        $app: "t.krv.ord3@1.0.0"
        $model: {
          rings: { $key: "id", id: "text", algorithm: "text" }
          $public: { ring: { $view: ".rings { $key: algorithm, algorithm, n: count(group) }" } }
        }
        $data: { rings: { r1: { algorithm: "Ed25519" } } }
      }
      steps: [
        { watch: "public.ring", id: "w1", expect_init: { value: [ { algorithm: "Ed25519", n: "1" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-ordinary-group");
}

// ── DRY 1 (charge #2) ────────────────────────────────────────────────────────
// §10.5 + §18.1: a blob descriptor member read inside a scoped-role recursive
// `$where` predicate (`child.report.$bytes >= 5`) is readable, and coverage stays
// correct: the descendant `big` (`$bytes == 10`, satisfies `$where`) is surfaced,
// while `small` (`$bytes == 2`, fails `$where`) is pruned (§10.5 allow-list). The
// descriptors are seeded as ordinary composite values (§18.1: "The complete
// descriptor is the application value"); reading `$bytes` needs no placement.
// This HOLDS — recorded DRY, no divergence.
#[test]
fn dry_blob_descriptor_in_recursive_where_holds() {
    // 128 lowercase-hex SHA-512 stand-ins (§18.1); distinct per descriptor.
    let h0 = "0".repeat(128);
    let h1 = format!("{}1", "0".repeat(127));
    let h2 = format!("{}2", "0".repeat(127));
    let text = format!(
        r##"{{
      format: 1
      name: dry-blob-recursive-where
      suite: scenario
      spec: ["#interfaces", "§10.5", "#blobs", "§18.1"]
      package: {{
        $liasse: 1
        $app: "t.krv.recwhere@1.0.0"
        $model: {{
          accounts: {{ $key: "id", id: "text" }}
          companies: {{
            $key: "id"
            id: "text"
            name: "text"
            report: {{ $type: "blob", $max_bytes: "10485760", $media: ["text/plain"] }}
            subcompanies: {{ $like: "^" }}
            members: {{ $key: "account", account: {{ $ref: "/accounts" }}, admin: "bool = false" }}
            $roles: {{
              admin: {{
                $auth: "token"
                $members: ".members[:m | m.admin].account"
                company: {{
                  $view: ". {{ id, name }}"
                  $recursive: {{
                    $field: "subcompanies"
                    $through: ".subcompanies"
                    $bind: "child"
                    $where: "child.report.$bytes >= 5"
                  }}
                }}
              }}
            }}
          }}
          $auth: {{ token: {{ $credential: "text", $verify: "$credential", $actor: "/accounts[$proof]" }} }}
        }}
        $data: {{
          accounts: {{ alice: {{}} }}
          companies: {{
            root: {{
              name: "Root"
              report: {{ $sha512: "{h0}", $bytes: "5", $media: "text/plain" }}
              members: {{ alice: {{ admin: true }} }}
              subcompanies: {{
                big: {{ name: "Big", report: {{ $sha512: "{h1}", $bytes: "10", $media: "text/plain" }} }}
                small: {{ name: "Small", report: {{ $sha512: "{h2}", $bytes: "2", $media: "text/plain" }} }}
              }}
            }}
          }}
        }}
      }}
      steps: [
        {{ connect: "c1", authenticate: {{ role: "admin", auth: "token", credential: "alice" }} }}
        {{ watch: "admin.company", scope: "root", id: "w1",
          expect_init: {{ value: {{
            id: "root"
            name: "Root"
            subcompanies: [ {{ id: "big", name: "Big", "...": true }} ]
            "...": true
          }} }} }}
      ]
    }}"##
    );
    assert_all_pass(&run_case_text(&text), "dry-blob-recursive-where");
}

// ── DRY 2 (charge #3) ────────────────────────────────────────────────────────
// §18.1/§18.5 + §7.2/§7.5: aggregating a blob descriptor member directly inside a
// grouped view — `sum(group.file.$bytes)` — is statically rejected ("`file` is a
// blob, not a nested collection"). This is the SAME already-documented seam as
// the §18.11 billing view `sum(.uploads[:u|...].file.$bytes)`, allow-listed at
// `crates/liasse-testkit/src/scenario_gate.rs:308` — a KNOWN gap, not a NEW
// finding. The projected-member workaround `sum(group { z: .file.$bytes }.z)`
// loads and computes the grouped sizes, which this control pins (§18.1: staged
// content "hi"/"hello" = 2/5 UTF-8 bytes; §7.5 sum over int).
#[test]
fn dry_blob_descriptor_grouped_aggregate_workaround_holds() {
    let text = r##"{
      format: 1
      name: dry-blob-grouped-aggregate-workaround
      suite: scenario
      spec: ["#blobs", "§18.1", "§18.5", "#views", "§7.2", "§7.5"]
      hosts: { connectors: { "fs-a": { capabilities: ["stream_upload", "checksum"] } } }
      package: {
        $liasse: 1
        $app: "t.krv.blobgrp@1.0.0"
        $model: {
          stores: { $key: "id", id: "text", connector: "text", enabled: "bool = true" }
          docs: {
            $key: "id"
            $blob_storage: { $in: "/stores['primary']" }
            id: "text"
            kind: "text"
            file: { $type: "blob", $max_bytes: "10485760", $media: ["text/plain"] }
          }
          $mut: { add: [ "d = .docs + { id: @id, kind: @kind, file: @file }", "return d { id }" ] }
          bykind: { $view: ".docs { $key: kind, kind, bytes: sum(group { z: .file.$bytes }.z) }" }
          $public: { docs: { $view: ".bykind", $mut: { add: ".add" } } }
        }
        $data: { stores: { primary: { connector: "fs-a" } } }
      }
      steps: [
        { connect: "c1" }
        { blob_put: { call: "public.docs.add", param: "file", args: { id: "d1", kind: "a" }, content: "hi", media: "text/plain", on: "c1" }, expect: { outcome: ok, value: { id: "d1" } } }
        { blob_put: { call: "public.docs.add", param: "file", args: { id: "d2", kind: "a" }, content: "hello", media: "text/plain", on: "c1" }, expect: { outcome: ok, value: { id: "d2" } } }
        { blob_put: { call: "public.docs.add", param: "file", args: { id: "d3", kind: "b" }, content: "xyz", media: "text/plain", on: "c1" }, expect: { outcome: ok, value: { id: "d3" } } }
        { watch: "public.docs", on: "c1", id: "w1", expect_init: { value: [ { kind: "a", bytes: "7" }, { kind: "b", bytes: "3" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "dry-blob-grouped-aggregate-workaround");
}
