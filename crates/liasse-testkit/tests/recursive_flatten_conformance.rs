//! Recursive-flatten conformance and capability probe.
//!
//! Establishes how a RECURSIVE FLATTENING view over a self-referential hierarchy
//! is expressed with CURRENT spec constructs, and which of those constructs the
//! runtime actually materializes. Two families are exercised:
//!
//! - the FIXED-depth traversal-flatten (§6.4) — `.a[:x].b[:y].c[:z] { … }` — which
//!   flattens a bounded number of nested levels into flat projected rows; and
//! - the ARBITRARY-depth vehicles the spec provides over a single self-referential
//!   shape: the §5.8 nested recursive shape (`subcompanies: "company"` / `$like:"^"`)
//!   traversed by §6.4, and the §10.5 `$recursive` surface coverage that re-applies
//!   one scoped-role surface to every included descendant to a fixed point.
//!
//! The passing tests are green conformance: they lock forms the runtime serves
//! today. The `#[ignore]`d tests are HELD REPROS for spec'd-but-unimplemented
//! mechanisms — each ignore reason states the §clause, the expected-vs-observed
//! divergence, and the root-cause file:line. They assert the SPEC-correct result,
//! so `cargo test -p liasse-testkit -- --ignored` reproduces each gap, while the
//! default gate stays green (they are skipped). Every expectation is deducible
//! from SPEC.md text alone.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

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

// ===========================================================================
// GREEN — forms the runtime materializes today.
// ===========================================================================

/// §6.4/§7.2/§5.4/§B.5: a THREE-level fixed-depth traversal-flatten over distinct
/// nested keyed collections returns one flat row per leaf, carrying the full
/// ancestor+local composite identity, in ancestor-then-local key order. The
/// repeated local key `eu` under three different ancestors yields three distinct
/// rows — the nested-row identity is the ancestor+local composite (§5.4/D.1).
#[test]
fn fixed_depth_traversal_flatten_distinct_collections() {
    let case = r##"{
      format: 1
      name: flat3
      suite: scenario
      spec: ["#expressions", "§6.4", "§7.2", "#state-model", "§5.4"]
      package: {
        $liasse: 1
        $app: "t.flat3@1.0.0"
        $model: {
          companies: {
            $key: "id", id: "text"
            divisions: {
              $key: "id", id: "text"
              teams: { $key: "id", id: "text", name: "text" }
            }
          }
          $public: { leaves: {
            $view: ".companies[:c].divisions[:d].teams[:t] { company: c.id, division: d.id, team: t.id, name: t.name }"
          } }
        }
        $data: {
          companies: {
            acme: { divisions: {
              labs: { teams: { eu: { name: "Acme Labs EU" }, us: { name: "Acme Labs US" } } }
              ops:  { teams: { eu: { name: "Acme Ops EU" } } }
            } }
            globex: { divisions: { labs: { teams: { eu: { name: "Globex Labs EU" } } } } }
          }
        }
      }
      steps: [
        { watch: "public.leaves", id: "w1", expect_init: { value: [
          { company: "acme",   division: "labs", team: "eu", name: "Acme Labs EU" }
          { company: "acme",   division: "labs", team: "us", name: "Acme Labs US" }
          { company: "acme",   division: "ops",  team: "eu", name: "Acme Ops EU" }
          { company: "globex", division: "labs", team: "eu", name: "Globex Labs EU" }
        ] } }
      ]
    }"##;
    assert_pass(&run(case, "flat3"), "fixed-depth distinct-collection traversal flatten");
}

/// §7.1/§7.3: a self-referential hierarchy modelled as a flat adjacency list — one
/// keyed collection whose rows carry an optional `parent` self-link — is flattened
/// at ARBITRARY depth by a plain projected read. Every node at every depth is a
/// top-level row, so `.companies { id, name, parent }` returns the whole four-level
/// tree; the parent link keeps the hierarchy reconstructable from the flat result.
/// This is the runtime-viable arbitrary-depth flatten with current constructs.
#[test]
fn adjacency_list_flatten_arbitrary_depth() {
    let case = r##"{
      format: 1
      name: adj
      suite: scenario
      spec: ["#views", "§7.1", "§7.3"]
      package: {
        $liasse: 1
        $app: "t.adj@1.0.0"
        $model: {
          companies: { $key: "id", id: "text", name: "text", parent: "text?" }
          $public: { tree: { $view: ".companies { id, name, parent }" } }
        }
        $data: { companies: {
          acme: { name: "Acme" }
          labs: { name: "Labs", parent: "acme" }
          "labs-eu": { name: "Labs EU", parent: "labs" }
          rnd: { name: "R&D", parent: "labs-eu" }
        } }
      }
      steps: [
        { watch: "public.tree", id: "w1", expect_init: { value: [
          { id: "acme",    name: "Acme",    "...": true }
          { id: "labs",    name: "Labs",    parent: "acme" }
          { id: "labs-eu", name: "Labs EU", parent: "labs" }
          { id: "rnd",     name: "R&D",     parent: "labs-eu" }
        ] } }
      ]
    }"##;
    assert_pass(&run(case, "adj"), "adjacency-list arbitrary-depth flatten");
}

// ===========================================================================
// FINDINGS (held repros) — spec'd mechanisms the runtime does not materialize.
// Each asserts the SPEC-correct result and is #[ignore]d so the shared gate
// stays green; `cargo test -- --ignored` reproduces the divergence.
// ===========================================================================

/// FINDING F1 — §5.8 + §6.4: fixed-depth traversal-flatten over a SELF-REFERENTIAL
/// shape. Identical in form to the distinct-collection flatten above, but the
/// nested collection is the SAME named type (`subcompanies: "company"`). The
/// package compiles; at watch time the runtime's environment builder cannot shape
/// the self-referential nested field.
/// Expected: rows `[{ parent: acme, sub: labs, name: "Acme Labs" }]`.
/// Observed: the watch step is SKIPPED — host fault "engine invariant violated:
/// environment supplied a value that is not a row with this field".
/// Root cause: self-referential nested-collection field shaping in the runtime
/// evaluation environment (liasse-runtime environment/shape builder — the same
/// seam that skips `tests/05-state-model/common/named-type-recursive-shape.hjson`
/// and `like-recursion-adopts-containing-shape.hjson`).
#[test]
#[ignore = "FINDING F1: §5.8/§6.4 self-referential nested-collection view shaping unimplemented (host fault: 'not a row with this field'); repro-only"]
fn selfref_fixed_depth_traversal_flatten() {
    let case = r##"{
      format: 1
      name: selfref-flat
      suite: scenario
      spec: ["#state-model", "§5.8", "#expressions", "§6.4"]
      package: {
        $liasse: 1
        $app: "t.srf@1.0.0"
        $types: { company: { $key: "id", id: "text", name: "text", subcompanies: "company" } }
        $model: {
          companies: "company"
          $public: { flat: {
            $view: ".companies[:c].subcompanies[:s] { parent: c.id, sub: s.id, name: s.name }"
          } }
        }
        $data: { companies: { acme: { name: "Acme", subcompanies: { labs: { name: "Acme Labs" } } } } }
      }
      steps: [
        { watch: "public.flat", id: "w1", expect_init: { value: [
          { parent: "acme", sub: "labs", name: "Acme Labs" }
        ] } }
      ]
    }"##;
    assert_pass(&run(case, "selfref-flat"), "self-referential fixed-depth traversal flatten");
}

/// FINDING F2 — §10.5: `$recursive` surface coverage at full depth. A scoped role
/// propagates one surface through the self-referential `subcompanies` relation;
/// §10.5 requires the output to appear under `$field` as a nested keyed tree in
/// which every included descendant (four levels deep here) is surfaced with the
/// same projection. The `$recursive` block type-checks at load
/// (liasse-model/src/surface.rs:187 `check_recursive`), but the runtime never
/// materializes it.
/// Expected: the nested `{ id, name, subcompanies: [ … ] }` tree to full depth.
/// Observed: the watch produces NO view value ("expected a view value, none
/// observed").
/// Root cause: the surface compiler reads only `$view` and drops role `$`-members,
/// so `$recursive` is never expanded — liasse-runtime/src/compiled.rs:1650-1655
/// (role `$`-members skipped) and compile_one_surface_view (only `$view` compiled).
#[test]
#[ignore = "FINDING F2: §10.5 $recursive coverage validated at load but not materialized at runtime (compiled.rs:1650-1655); repro-only"]
fn recursive_coverage_full_depth() {
    let case = RECURSIVE_TREE_PACKAGE_HEADER.to_owned()
        + r##"
        $data: {
          accounts: { alice: {} }
          companies: { acme: {
            name: "Acme", members: { alice: { admin: true } }
            subcompanies: { labs: {
              name: "Labs"
              subcompanies: { "labs-eu": {
                name: "Labs EU"
                subcompanies: { rnd: { name: "R&D" } }
              } }
            } }
          } }
        }
      }
      steps: [
        { connect: "c1", authenticate: { role: "admin", auth: "token", credential: "alice" } }
        { watch: "admin.company", scope: "acme", id: "w1", expect_init: { value: {
          id: "acme", name: "Acme", plan: "active"
          subcompanies: [ {
            id: "labs", name: "Labs", plan: "active"
            subcompanies: [ {
              id: "labs-eu", name: "Labs EU", plan: "active"
              subcompanies: [ { id: "rnd", name: "R&D", plan: "active", "...": true } ]
              "...": true
            } ]
            "...": true
          } ]
          "...": true
        } } }
      ]
    }"##;
    assert_pass(&run(&case, "rec-full-depth"), "$recursive full-depth coverage");
}

/// FINDING F3 — §10.5: `$recursive` `$where`/`$except` pruning at depth. `$where`
/// is a hereditary allow-list and `$except` a hereditary deny-list: a node the
/// predicate excludes contributes no slot and NONE of its descendants are
/// surfaced or reparented. Here `closed` (fails `$where`) and `hr` (matched by
/// `$except`) must vanish WITH their whole subtrees, while `keep` under the
/// included `active` node remains. Fails for the same reason as F2 (no
/// materialization).
#[test]
#[ignore = "FINDING F3: §10.5 $recursive $where/$except pruning not materialized at runtime (compiled.rs:1650-1655); repro-only"]
fn recursive_coverage_prunes_subtrees_at_depth() {
    let case = RECURSIVE_TREE_PACKAGE_HEADER_PRUNED.to_owned()
        + r##"
        $data: {
          accounts: { alice: {} }
          companies: { acme: {
            name: "Acme", members: { alice: { admin: true } }
            subcompanies: {
              active: {
                name: "Active"
                subcompanies: { keep: { name: "Keep" } }
              }
              closed: {
                name: "Closed", plan: "closed"
                subcompanies: { orphan: { name: "Orphan under $where-excluded parent" } }
              }
              hr: {
                name: "HR"
                subcompanies: { hrsub: { name: "Orphan under $except-pruned parent" } }
              }
            }
          } }
        }
      }
      steps: [
        { connect: "c1", authenticate: { role: "admin", auth: "token", credential: "alice" } }
        // Only `active` (and its child `keep`) survive; `closed` fails $where and
        // `hr` matches $except, so both branches — and their descendants — are absent.
        { watch: "admin.company", scope: "acme", id: "w1", expect_init: { value: {
          id: "acme", name: "Acme", plan: "active"
          subcompanies: [ {
            id: "active", name: "Active", plan: "active"
            subcompanies: [ { id: "keep", name: "Keep", plan: "active", "...": true } ]
            "...": true
          } ]
          "...": true
        } } }
      ]
    }"##;
    assert_pass(&run(&case, "rec-prune"), "$recursive $where/$except pruning at depth");
}

/// FINDING F4 — §10.5: a single-node tree. The role-holding row has no included
/// descendants, so the surface projects just that node with an empty `$field`
/// nested view. Fails identically (no view value).
#[test]
#[ignore = "FINDING F4: §10.5 $recursive single-node coverage not materialized at runtime (compiled.rs:1650-1655); repro-only"]
fn recursive_coverage_single_node() {
    let case = RECURSIVE_TREE_PACKAGE_HEADER.to_owned()
        + r##"
        $data: {
          accounts: { alice: {} }
          companies: { acme: { name: "Acme", members: { alice: { admin: true } } } }
        }
      }
      steps: [
        { connect: "c1", authenticate: { role: "admin", auth: "token", credential: "alice" } }
        { watch: "admin.company", scope: "acme", id: "w1", expect_init: { value: {
          id: "acme", name: "Acme", plan: "active", subcompanies: [], "...": true
        } } }
      ]
    }"##;
    assert_pass(&run(&case, "rec-single"), "$recursive single-node coverage");
}

/// FINDING F5 — §10.5: an included non-leaf node whose covered child is itself a
/// leaf (empty children). The leaf's `$field` nested view is empty (`[]`). Fails
/// identically (no view value).
#[test]
#[ignore = "FINDING F5: §10.5 $recursive empty-children-leaf coverage not materialized at runtime (compiled.rs:1650-1655); repro-only"]
fn recursive_coverage_empty_children_leaf() {
    let case = RECURSIVE_TREE_PACKAGE_HEADER.to_owned()
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
        { watch: "admin.company", scope: "acme", id: "w1", expect_init: { value: {
          id: "acme", name: "Acme", plan: "active"
          subcompanies: [ { id: "labs", name: "Labs", plan: "active", subcompanies: [], "...": true } ]
          "...": true
        } } }
      ]
    }"##;
    assert_pass(&run(&case, "rec-empty-leaf"), "$recursive empty-children leaf");
}

/// FINDING F6 — §10.5/§10.3: addressing a `$recursive`-covered descendant receiver
/// by the role handle plus its key path. §10.5 propagates "the same surface
/// projection AND mutations" to included children; admission re-walks the recursive
/// relation along the whole path and binds the addressed descendant as the mutation
/// `.` receiver (the role-holding row is the empty path).
/// Expected: renaming the covered subcompany `labs` under scope `acme` succeeds.
/// Observed: `denied` — scoped-role addressing (even non-recursive, §10.3) is
/// unwired this phase, so `scope`/`descendant` never bind a receiver.
/// Root cause: scoped-role (row-nested role) admission/addressing is not threaded
/// through the surface host call path (same seam that denies
/// `tests/10-interfaces-roles/common/scoped-role-addressed-by-row-and-name.hjson`).
#[test]
#[ignore = "FINDING F6: §10.5/§10.3 scoped-role + $recursive descendant addressing unwired (observed `denied`); repro-only"]
fn recursive_descendant_addressing() {
    let case = r##"{
      format: 1
      name: rec-addr
      suite: scenario
      spec: ["#interfaces", "§10.5", "§10.3"]
      package: {
        $liasse: 1
        $app: "t.reca@1.0.0"
        $model: {
          accounts: { $key: "id", id: "text" }
          companies: {
            $key: "id", id: "text", name: "text", plan: "text = 'active'"
            subcompanies: { $like: "^" }
            members: { $key: "account", account: { $ref: "/accounts" }, admin: "bool = false" }
            $mut: { rename: [ ".name = @name", "return . { id, name }" ] }
            $roles: { admin: {
              $auth: "token"
              $members: ".members[:m | m.admin].account"
              company: {
                $view: ". { id, name, plan }"
                $mut: { rename: ".rename" }
                $recursive: { $field: "subcompanies", $through: ".subcompanies", $bind: "child" }
              }
            } }
          }
          $auth: { token: { $credential: "text", $verify: "$credential", $actor: "/accounts[$proof]" } }
        }
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
        { call: "admin.company.rename", scope: "acme", descendant: "labs",
          args: { name: "Labs 2" }, on: "c1",
          expect: { outcome: ok, value: { id: "labs", name: "Labs 2" } } }
      ]
    }"##;
    assert_pass(&run(case, "rec-addr"), "$recursive descendant mutation addressing");
}

/// The shared head of a `$recursive` role package: a self-referential `companies`
/// shape with an `admin` role scoped per company row, propagating the `. { id,
/// name, plan }` surface through `subcompanies`. Each finding appends its own
/// `$data` and `steps`.
const RECURSIVE_TREE_PACKAGE_HEADER: &str = r##"{
  format: 1
  name: rec-tree
  suite: scenario
  spec: ["#interfaces", "§10.5", "§10.3"]
  package: {
    $liasse: 1
    $app: "t.rect@1.0.0"
    $model: {
      accounts: { $key: "id", id: "text" }
      companies: {
        $key: "id", id: "text", name: "text", plan: "text = 'active'"
        subcompanies: { $like: "^" }
        members: { $key: "account", account: { $ref: "/accounts" }, admin: "bool = false" }
        $roles: { admin: {
          $auth: "token"
          $members: ".members[:m | m.admin].account"
          company: {
            $view: ". { id, name, plan }"
            $recursive: { $field: "subcompanies", $through: ".subcompanies", $bind: "child" }
          }
        } }
      }
      $auth: { token: { $credential: "text", $verify: "$credential", $actor: "/accounts[$proof]" } }
    }"##;

/// Same as [`RECURSIVE_TREE_PACKAGE_HEADER`] but the `$recursive` block carries a
/// `$where` allow-list (`plan != 'closed'`) and an `$except` deny-list
/// (`id == 'hr'`) — the pruning predicates F3 exercises.
const RECURSIVE_TREE_PACKAGE_HEADER_PRUNED: &str = r##"{
  format: 1
  name: rec-tree-pruned
  suite: scenario
  spec: ["#interfaces", "§10.5", "§10.3"]
  package: {
    $liasse: 1
    $app: "t.rectp@1.0.0"
    $model: {
      accounts: { $key: "id", id: "text" }
      companies: {
        $key: "id", id: "text", name: "text", plan: "text = 'active'"
        subcompanies: { $like: "^" }
        members: { $key: "account", account: { $ref: "/accounts" }, admin: "bool = false" }
        $roles: { admin: {
          $auth: "token"
          $members: ".members[:m | m.admin].account"
          company: {
            $view: ". { id, name, plan }"
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
