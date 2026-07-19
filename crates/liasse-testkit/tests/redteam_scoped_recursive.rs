//! RED TEAM — adversarial attack on §10.3/§10.5 scoped-role coverage + recursive
//! descendant addressing (F1-F6, commits 30a1877/dcea3f2/15473ac).
//!
//! AUTH-FIRST: every test here probes an authorization or oracle property of the
//! scoped-role machinery. The goal is to break, not to conform — each `expect:
//! denied` control asserts a SECURITY boundary (no cross-scope read/write, no
//! addressing of a pruned/excluded descendant, uniform §10.4 denial), and each
//! `expect: ok` control is a MUST-ALLOW guard so the boundary is not just a blanket
//! deny. A test that fails is a finding; a green suite is convergence evidence.
//!
//! Every expectation is deducible from SPEC.md §10.3/§10.4/§10.5, §5.4/§5.8, §12.2
//! text alone — never from the implementation's own output.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

/// Run one inline scenario case through the real runtime + surface stack over an
/// in-memory store. `scope`/`descendant` are allowed step members (§10 NOTES).
fn run(case_text: &str, name: &str) -> CaseResult {
    let allowed: BTreeSet<String> =
        ["scope".to_owned(), "descendant".to_owned()].into_iter().collect();
    let case = Case::from_hjson(case_text, Path::new(name), &allowed).expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new(name), SuiteKind::Common, &case)
}

/// Assert the whole case reached a clean pass, printing the first divergence.
fn assert_pass(result: &CaseResult, context: &str) {
    let first = result.steps.iter().find(|s| !s.result.is_pass());
    assert!(
        result.verdict.is_pass(),
        "{context}: expected a clean pass, got {:?}; first divergence: {:?}",
        result.verdict,
        first.map(|s| (s.index, &s.action, &s.result)),
    );
}

/// The shared head of a `$recursive` scoped-role package carrying BOTH a `rename`
/// (name mutation) and a `setplan` (plan mutation) covered call, and the hereditary
/// `$where` (`plan != 'closed'`) / `$except` (`id == 'hr'`) pruning predicates. Each
/// test appends its own `$data` and `steps`. Data that names nothing `hr` and sets
/// no `closed` plan is fully included, so the pruning predicates are inert there.
const PKG: &str = r##"{
  format: 1
  name: rt-scoped
  suite: scenario
  spec: ["#interfaces", "§10.5", "§10.3", "§10.4", "§5.4"]
  package: {
    $liasse: 1
    $app: "t.rtsc@1.0.0"
    $model: {
      accounts: { $key: "id", id: "text" }
      companies: {
        $key: "id", id: "text", name: "text", plan: "text = 'active'"
        subcompanies: { $like: "^" }
        members: { $key: "account", account: { $ref: "/accounts" }, admin: "bool = false" }
        $mut: {
          rename: [ ".name = @name", "return . { id, name }" ]
          setplan: [ ".plan = @plan", "return . { id, plan }" ]
        }
        $roles: { admin: {
          $auth: "token"
          $members: ".members[:m | m.admin].account"
          company: {
            $view: ". { id, name, plan }"
            $mut: { rename: ".rename", setplan: ".setplan" }
            $recursive: {
              $field: "subcompanies", $through: ".subcompanies", $bind: "child"
              $where: "child.plan != 'closed'"
              $except: "child.id == 'hr'"
            }
          }
        } }
      }
      $auth: { token: { $credential: "text", $verify: "$credential", $actor: "/accounts[$proof]" } }
    }"##;

// ===========================================================================
// ATTACK 3 — descendant addressing must AGREE with the coverage view's pruning:
// a $where-excluded or $except-pruned descendant is NOT an addressable mutation
// receiver, and neither is any node behind a pruned ancestor (hereditary).
// ===========================================================================

/// §10.5/§10.4: addressing a `$where`-excluded (`closed`) or `$except`-pruned
/// (`hr`) descendant as a mutation receiver is DENIED — uniformly, the same
/// unresolvable-name outcome as a nonexistent address. The INCLUDED sibling
/// `active` renames fine (must-allow control: the deny is a boundary, not a
/// blanket refusal).
#[test]
fn addressing_denies_pruned_descendants_allows_included() {
    let case = PKG.to_owned()
        + r##"
        $data: {
          accounts: { alice: {} }
          companies: { acme: {
            name: "Acme", members: { alice: { admin: true } }
            subcompanies: {
              active: { name: "Active" }
              closed: { name: "Closed", plan: "closed" }
              hr: { name: "HR" }
            }
          } }
        }
      }
      steps: [
        { connect: "c1", authenticate: { role: "admin", auth: "token", credential: "alice" } }
        // Included sibling: MUST allow.
        { call: "admin.company.rename", scope: "acme", descendant: "active",
          args: { name: "Active 2" }, on: "c1",
          expect: { outcome: ok, value: { id: "active", name: "Active 2" } } }
        // $where-excluded: MUST deny (it does not appear in the covered tree).
        { call: "admin.company.rename", scope: "acme", descendant: "closed",
          args: { name: "hax" }, on: "c1", expect: { outcome: denied } }
        // $except-pruned: MUST deny.
        { call: "admin.company.rename", scope: "acme", descendant: "hr",
          args: { name: "hax" }, on: "c1", expect: { outcome: denied } }
      ]
    }"##;
    assert_pass(&run(&case, "addr-prune"), "pruned descendants are not addressable");
}

/// §10.5 hereditary pruning of the ADDRESSING walk: a deep node that would be
/// included on its own is UNREACHABLE through a pruned mid ancestor. `orphan` sits
/// under `$where`-excluded `closed`; `hrsub` under `$except`-pruned `hr`. Both deep
/// addresses deny at the pruned mid-step. A fully-included deep chain
/// (`active -> keep`) addresses fine (must-allow control).
#[test]
fn addressing_denies_through_pruned_midnode() {
    let case = PKG.to_owned()
        + r##"
        $data: {
          accounts: { alice: {} }
          companies: { acme: {
            name: "Acme", members: { alice: { admin: true } }
            subcompanies: {
              active: { name: "Active", subcompanies: { keep: { name: "Keep" } } }
              closed: { name: "Closed", plan: "closed", subcompanies: { orphan: { name: "Orphan" } } }
              hr: { name: "HR", subcompanies: { hrsub: { name: "HR Sub" } } }
            }
          } }
        }
      }
      steps: [
        { connect: "c1", authenticate: { role: "admin", auth: "token", credential: "alice" } }
        // Fully-included deep chain: MUST allow.
        { call: "admin.company.rename", scope: "acme", descendant: [ "active", "keep" ],
          args: { name: "Keep 2" }, on: "c1",
          expect: { outcome: ok, value: { id: "keep", name: "Keep 2" } } }
        // Deep node behind a $where-excluded ancestor: MUST deny (hereditary).
        { call: "admin.company.rename", scope: "acme", descendant: [ "closed", "orphan" ],
          args: { name: "hax" }, on: "c1", expect: { outcome: denied } }
        // Deep node behind a $except-pruned ancestor: MUST deny (hereditary).
        { call: "admin.company.rename", scope: "acme", descendant: [ "hr", "hrsub" ],
          args: { name: "hax" }, on: "c1", expect: { outcome: denied } }
      ]
    }"##;
    assert_pass(&run(&case, "addr-hereditary"), "hereditary pruning of the addressing walk");
}

// ===========================================================================
// ATTACK 2/3 — every scoped denial collapses to ONE outcome class (§10.4). A
// nonexistent scope, a nonexistent descendant, and a wrong-key-type descendant
// are all `denied`, indistinguishable from a valid-but-pruned one.
// ===========================================================================

/// §10.4 uniform denial: a scope naming no live row, a descendant key naming no
/// child, and a wrong-typed descendant key each collapse to the same `denied`
/// class as the pruning denials above — no existence oracle by outcome class.
#[test]
fn nonexistent_scope_and_descendant_are_uniformly_denied() {
    let case = PKG.to_owned()
        + r##"
        $data: {
          accounts: { alice: {} }
          companies: { acme: {
            name: "Acme", members: { alice: { admin: true } }
            subcompanies: { labs: { name: "Labs" } }
          } }
        }
      }
      steps: [
        { connect: "c1", authenticate: { role: "admin", auth: "token", credential: "alice" } }
        // Scope names no live row.
        { call: "admin.company.rename", scope: "ghost", args: { name: "hax" }, on: "c1",
          expect: { outcome: denied } }
        // Descendant names no child under the (live, held) scope.
        { call: "admin.company.rename", scope: "acme", descendant: "ghost",
          args: { name: "hax" }, on: "c1", expect: { outcome: denied } }
        // Wrong-typed descendant key (a number where text keys the relation).
        { call: "admin.company.rename", scope: "acme", descendant: 42,
          args: { name: "hax" }, on: "c1", expect: { outcome: denied } }
      ]
    }"##;
    assert_pass(&run(&case, "addr-uniform"), "nonexistent scope/descendant uniformly denied");
}

// ===========================================================================
// ATTACK 1 — cross-scope. A holder of the scoped role on row A must be denied
// EVERY access to row B: a scope B call, and a descendant address that would
// escape A's subtree.
// ===========================================================================

/// §10.3/§10.5: cross-scope calls deny. Alice holds `admin` of `acme`, bob of
/// `globex`. Alice calling scope `globex` is denied (she is not a member of that
/// scope row). Alice cannot reach `globex` as a descendant of `acme` either
/// (`globex` is a top-level company, not a subcompany of `acme`). Bob's own-scope
/// call is the must-allow control.
#[test]
fn cross_scope_call_is_denied() {
    let case = PKG.to_owned()
        + r##"
        $data: {
          accounts: { alice: {}, bob: {} }
          companies: {
            acme:   { name: "Acme",   members: { alice: { admin: true } } }
            globex: { name: "Globex", members: { bob:   { admin: true } } }
          }
        }
      }
      steps: [
        { connect: "c1", authenticate: { role: "admin", auth: "token", credential: "alice" } }
        { connect: "c2", authenticate: { role: "admin", auth: "token", credential: "bob" } }
        // Cross-scope: alice (scoped acme) addresses globex's scope row -> deny.
        { call: "admin.company.rename", scope: "globex", args: { name: "hax" }, on: "c1",
          expect: { outcome: denied } }
        // Cross-scope escape: globex is not a subcompany of acme -> deny.
        { call: "admin.company.rename", scope: "acme", descendant: "globex",
          args: { name: "hax" }, on: "c1", expect: { outcome: denied } }
        // Must-allow control: bob renames his own scope row.
        { call: "admin.company.rename", scope: "globex", args: { name: "Globex 2" }, on: "c2",
          expect: { outcome: ok, value: { id: "globex", name: "Globex 2" } } }
      ]
    }"##;
    assert_pass(&run(&case, "addr-crossscope"), "cross-scope call denied");
}

// ===========================================================================
// ATTACK 4 — nested-row identity (§5.4/D.1): two descendants with the SAME local
// key under DIFFERENT parents are distinct rows. Addressing one must not touch
// the other; the covered tree must carry both.
// ===========================================================================

/// §5.4/§10.5: `na.shared` and `emea.shared` share a local key but are distinct
/// composite-identity rows. The coverage view carries both; addressing
/// `["na","shared"]` renames only that one, and a fresh read confirms
/// `emea.shared` is UNTOUCHED. A same-local-key collision here would be a
/// cross-row write.
#[test]
fn nested_identity_addressing_hits_exact_row() {
    let case = PKG.to_owned()
        + r##"
        $data: {
          accounts: { alice: {} }
          companies: { acme: {
            name: "Acme", members: { alice: { admin: true } }
            subcompanies: {
              na:   { name: "NA",   subcompanies: { shared: { name: "NA shared" } } }
              emea: { name: "EMEA", subcompanies: { shared: { name: "EMEA shared" } } }
            }
          } }
        }
      }
      steps: [
        { connect: "c1", authenticate: { role: "admin", auth: "token", credential: "alice" } }
        // Both distinct-identity `shared` nodes are surfaced under their own parent
        // (subcompanies in Annex B key order: `emea` before `na`).
        { watch: "admin.company", scope: "acme", id: "w1", on: "c1", expect_init: { value: {
          id: "acme", name: "Acme", plan: "active"
          subcompanies: [
            {
              id: "emea", name: "EMEA", plan: "active"
              subcompanies: [ { id: "shared", name: "EMEA shared", plan: "active", "...": true } ]
              "...": true
            }
            {
              id: "na", name: "NA", plan: "active"
              subcompanies: [ { id: "shared", name: "NA shared", plan: "active", "...": true } ]
              "...": true
            }
          ]
          "...": true
        } } }
        // Address exactly na.shared.
        { call: "admin.company.rename", scope: "acme", descendant: [ "na", "shared" ],
          args: { name: "NA renamed" }, on: "c1",
          expect: { outcome: ok, value: { id: "shared", name: "NA renamed" } } }
        // The covered tree now shows na.shared renamed and emea.shared UNTOUCHED.
        { expect_view: { watch: "w1", value: {
          id: "acme", name: "Acme", plan: "active"
          subcompanies: [
            {
              id: "emea", name: "EMEA", plan: "active"
              subcompanies: [ { id: "shared", name: "EMEA shared", plan: "active", "...": true } ]
              "...": true
            }
            {
              id: "na", name: "NA", plan: "active"
              subcompanies: [ { id: "shared", name: "NA renamed", plan: "active", "...": true } ]
              "...": true
            }
          ]
          "...": true
        } } }
      ]
    }"##;
    assert_pass(&run(&case, "nested-identity"), "nested-row identity addressing is exact");
}

// ===========================================================================
// ATTACK 5 — live behavior (§12.2). A covered coverage view must re-materialize
// correctly after a covered-descendant mutation and after a $where-predicate flip.
// ===========================================================================

/// §12.2: a live coverage subscription reflects a covered-descendant mutation on
/// the same connection (coarse re-init per #42 is expected; CORRECTNESS is not).
/// After renaming `labs`, the retained coverage shows the new name — no stale row.
#[test]
fn live_coverage_reflects_descendant_rename() {
    let case = PKG.to_owned()
        + r##"
        $data: {
          accounts: { alice: {} }
          companies: { acme: {
            name: "Acme", members: { alice: { admin: true } }
            subcompanies: { labs: { name: "Labs" } }
          } }
        }
      }
      steps: [
        { connect: "c1", authenticate: { role: "admin", auth: "token", credential: "alice" } }
        { watch: "admin.company", scope: "acme", id: "w1", on: "c1", expect_init: { value: {
          id: "acme", name: "Acme", plan: "active"
          subcompanies: [ { id: "labs", name: "Labs", plan: "active", subcompanies: [], "...": true } ]
          "...": true
        } } }
        { call: "admin.company.rename", scope: "acme", descendant: "labs",
          args: { name: "Labs 2" }, on: "c1",
          expect: { outcome: ok, value: { id: "labs", name: "Labs 2" } } }
        { expect_view: { watch: "w1", value: {
          id: "acme", name: "Acme", plan: "active"
          subcompanies: [ { id: "labs", name: "Labs 2", plan: "active", subcompanies: [], "...": true } ]
          "...": true
        } } }
      ]
    }"##;
    assert_pass(&run(&case, "live-rename"), "live coverage reflects descendant rename");
}

/// §10.5/§12.2 predicate flip: a covered descendant that is INCLUDED at address
/// time can be mutated to fail `$where`; afterwards it is pruned from the covered
/// tree AND is no longer addressable. `swing` starts active (included); setting
/// its plan to `closed` (a legal mutation of an included row) removes it from
/// coverage, and a second address then denies — the addressing set tracks the
/// live predicate, never a stale membership.
#[test]
fn predicate_flip_prunes_and_revokes_addressability() {
    let case = PKG.to_owned()
        + r##"
        $data: {
          accounts: { alice: {} }
          companies: { acme: {
            name: "Acme", members: { alice: { admin: true } }
            subcompanies: { swing: { name: "Swing" } }
          } }
        }
      }
      steps: [
        { connect: "c1", authenticate: { role: "admin", auth: "token", credential: "alice" } }
        { watch: "admin.company", scope: "acme", id: "w1", on: "c1", expect_init: { value: {
          id: "acme", name: "Acme", plan: "active"
          subcompanies: [ { id: "swing", name: "Swing", plan: "active", subcompanies: [], "...": true } ]
          "...": true
        } } }
        // `swing` is included -> addressable: close it.
        { call: "admin.company.setplan", scope: "acme", descendant: "swing",
          args: { plan: "closed" }, on: "c1",
          expect: { outcome: ok, value: { id: "swing", plan: "closed" } } }
        // Now $where excludes it: the covered tree drops it entirely.
        { expect_view: { watch: "w1", value: {
          id: "acme", name: "Acme", plan: "active", subcompanies: [], "...": true
        } } }
        // And it is no longer an addressable receiver (left the coverage).
        { call: "admin.company.rename", scope: "acme", descendant: "swing",
          args: { name: "hax" }, on: "c1", expect: { outcome: denied } }
      ]
    }"##;
    assert_pass(&run(&case, "predicate-flip"), "predicate flip prunes and revokes addressability");
}

// ===========================================================================
// CONTROL — the role-holding row (empty descendant path) is the covered `.` and
// is addressable; this anchors that the deny-tests above are boundaries, not a
// broken pipeline.
// ===========================================================================

/// §10.3/§10.5: the role-holding row itself (empty descendant path) is the covered
/// receiver; a scoped call with no descendant renames the scope row.
#[test]
fn role_holding_row_is_addressable() {
    let case = PKG.to_owned()
        + r##"
        $data: {
          accounts: { alice: {} }
          companies: { acme: {
            name: "Acme", members: { alice: { admin: true } }
            subcompanies: { labs: { name: "Labs" } }
          } }
        }
      }
      steps: [
        { connect: "c1", authenticate: { role: "admin", auth: "token", credential: "alice" } }
        { call: "admin.company.rename", scope: "acme", args: { name: "Acme 2" }, on: "c1",
          expect: { outcome: ok, value: { id: "acme", name: "Acme 2" } } }
      ]
    }"##;
    assert_pass(&run(&case, "role-row"), "role-holding row is addressable");
}
