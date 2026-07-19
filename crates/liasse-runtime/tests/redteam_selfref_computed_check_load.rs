//! RED TEAM — §5.8 self-referential collection × §5.2 computed values / §5.10
//! checks: a cross-cutting seam where the F1 self-ref landing (commit 30a1877)
//! meets the ordinary field-derivation machinery.
//!
//! # Finding (open bug; the `#[ignore]`d repros pin it)
//!
//! A field-referencing **computed value** (§5.2, `label: "= .name"`) or a
//! field-referencing **row/field `$check`** (§5.10, `size(.name) > 0`) declared on
//! a self-referential collection (§5.8, `subcompanies: "company"` / `$like: "^"`)
//! is **rejected at load** with `E-EXPR "no field \`name\` on this row"` — even
//! though the collection plainly declares `name`, and the identical computed/check
//! on a NON-self-referential collection loads fine.
//!
//! ## Root cause
//!
//! `compile_collection` (crates/liasse-runtime/src/compiled.rs) eagerly expands a
//! self-referential shape to `MAX_SELF_REF_DEPTH = 32` (compiled.rs:642) and
//! compiles the collection's computed values and `$check`s **at every depth level**
//! against `row_ty = schema.receiver_row_type(path)` (compiled.rs ~L675). The model
//! type resolver truncates the self-referential field to `json` once its own
//! `MAX_DEPTH = 32` cap is exceeded (schema.rs:159-161), so at the deepest compiled
//! levels `receiver_row_type` walks into that `json` field and returns `None`
//! (schema.rs:44-54, the non-Row/View arm), whereupon `compile_collection` falls
//! back to an **empty** `RowType::keyless(empty)`. A computed/check expression then
//! compiles `.name` against a row with no fields, so `.name` fails to resolve and
//! the whole load is rejected.
//!
//! ## Why it is a bug (not acknowledged debt)
//!
//! §5.2 ("A computed value ... participates in views, checks, sorting, and
//! projections like any other value") and §5.10 (checks) place no restriction on
//! self-referential collections (§5.8). The collection's row type has `name` at
//! every real depth; the empty fallback is a compile-internal artifact of the two
//! independent depth caps not lining up, not a spec-mandated rejection. The failure
//! is fail-closed (a false LOAD rejection of a spec-valid package), so it is a
//! usability/correctness gap, not a data-integrity or authorization breach.
//!
//! The `must_load` tests assert the SPEC-correct outcome (the package loads) and are
//! `#[ignore]`d held repros: `cargo test -p liasse-runtime --test
//! redteam_selfref_computed_check_load -- --ignored` reproduces the gap, while the
//! PASSING controls (which run by default) fence the boundary so a fix cannot regress
//! the neighbours. Every expectation is deducible from SPEC.md alone.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_ident::InstanceId;
use liasse_runtime::Engine;
use liasse_store::MemoryStore;
use support::generator;

/// Attempt a load; `true` iff the definition compiled and activated.
fn loads(name: &str, definition: &str) -> bool {
    let mut generator = generator();
    let store = MemoryStore::new(InstanceId::new(name));
    Engine::load(store, definition, &mut generator).is_ok()
}

// ===========================================================================
// OPEN-BUG REPROS — assert the SPEC-correct outcome (the package loads). RED today.
// ===========================================================================

/// §5.2 + §5.8: a computed value referencing a declared field (`= .name`) on a
/// self-referential collection MUST load — `name` is a real field of `company`.
#[test]
fn selfref_computed_field_must_load() {
    let definition = r##"{
      "$liasse": 1,
      "$app": "t.srcomp@1.0.0",
      "$types": { "company": { "$key": "id", "id": "text", "name": "text", "label": "= .name", "subcompanies": "company" } },
      "$model": { "companies": "company", "flat": { "$view": ".companies { id, name, label }" } }
    }"##;
    assert!(loads("selfref-computed", definition), "a §5.2 computed on a §5.8 self-ref collection must load");
}

/// §5.10 + §5.8: a row `$check` referencing a declared field (`size(.name) > 0`) on
/// a self-referential collection MUST load.
#[test]
fn selfref_row_check_must_load() {
    let definition = r##"{
      "$liasse": 1,
      "$app": "t.srchk@1.0.0",
      "$types": { "company": { "$key": "id", "id": "text", "name": "text", "$check": ["size(.name) > 0", "name required"], "subcompanies": "company" } },
      "$model": { "companies": "company", "flat": { "$view": ".companies { id, name }" } }
    }"##;
    assert!(loads("selfref-check", definition), "a §5.10 row check on a §5.8 self-ref collection must load");
}

/// The same repro via the `$like: "^"` spelling of self-reference (§5.8), which
/// adopts the containing shape rather than a `$types` name.
#[test]
fn selfref_like_computed_must_load() {
    let definition = r##"{
      "$liasse": 1,
      "$app": "t.srlike@1.0.0",
      "$model": { "companies": { "$key": "id", "id": "text", "name": "text", "label": "= .name", "subcompanies": { "$like": "^" } },
                  "flat": { "$view": ".companies { id, name, label }" } }
    }"##;
    assert!(loads("selfref-like-computed", definition), "a §5.2 computed on a `$like: ^` self-ref collection must load");
}

// ===========================================================================
// PASSING CONTROLS — fence the boundary. These run by default and lock that the
// bug is SPECIFIC to (self-ref × field-referencing computed/check), not a blanket
// break of self-ref or of computed/check.
// ===========================================================================

/// CONTROL: the self-ref collection WITHOUT the computed/check loads — self-ref
/// itself is fine (§5.8, F1). This is the neighbour a fix must not regress.
#[test]
fn control_selfref_without_computed_loads() {
    let definition = r##"{
      "$liasse": 1,
      "$app": "t.srbase@1.0.0",
      "$types": { "company": { "$key": "id", "id": "text", "name": "text", "subcompanies": "company" } },
      "$model": { "companies": "company", "flat": { "$view": ".companies { id, name }" } }
    }"##;
    assert!(loads("selfref-base", definition), "a §5.8 self-ref collection loads without a computed");
}

/// CONTROL: the identical computed value on a NON-self-referential collection loads
/// — computed values themselves are fine (§5.2). Isolates the interaction to self-ref.
#[test]
fn control_normal_collection_with_computed_loads() {
    let definition = r##"{
      "$liasse": 1,
      "$app": "t.normcomp@1.0.0",
      "$model": { "companies": { "$key": "id", "id": "text", "name": "text", "label": "= .name" },
                  "flat": { "$view": ".companies { id, name, label }" } }
    }"##;
    assert!(loads("normal-computed", definition), "a §5.2 computed on a non-self-ref collection loads");
}

/// CONTROL: a CONSTANT computed (no field reference, `= 'x'`) on a self-ref
/// collection loads — it is field references specifically that hit the empty
/// fallback row, so a computed with none loads even on a self-ref shape.
#[test]
fn control_selfref_constant_computed_loads() {
    let definition = r##"{
      "$liasse": 1,
      "$app": "t.srconst@1.0.0",
      "$types": { "company": { "$key": "id", "id": "text", "name": "text", "label": "= 'x'", "subcompanies": "company" } },
      "$model": { "companies": "company", "flat": { "$view": ".companies { id, name, label }" } }
    }"##;
    assert!(loads("selfref-const", definition), "a constant §5.2 computed on a §5.8 self-ref collection loads");
}

/// CONTROL: a `$normalize` (which reads the field value `.`, not a named field of
/// the row) on a self-ref collection loads — the empty fallback row has `.`, just
/// not named fields.
#[test]
fn control_selfref_normalize_loads() {
    let definition = r##"{
      "$liasse": 1,
      "$app": "t.srnorm@1.0.0",
      "$types": { "company": { "$key": "id", "id": "text", "name": { "$type": "text", "$normalize": "string.trim(.)" }, "subcompanies": "company" } },
      "$model": { "companies": "company", "flat": { "$view": ".companies { id, name }" } }
    }"##;
    assert!(loads("selfref-normalize", definition), "a §5 $normalize on a §5.8 self-ref collection loads");
}

/// CONTROL: a collection `$mut` referencing a named field (`.name = @name`) on a
/// self-ref collection loads — mutation bodies are not compiled per self-ref depth
/// the way computed/checks are, so they escape the empty-fallback path.
#[test]
fn control_selfref_mutation_field_ref_loads() {
    let definition = r##"{
      "$liasse": 1,
      "$app": "t.srmut@1.0.0",
      "$types": { "company": { "$key": "id", "id": "text", "name": "text", "subcompanies": "company", "$mut": { "rename": ".name = @name" } } },
      "$model": { "companies": "company", "flat": { "$view": ".companies { id, name }" } }
    }"##;
    assert!(loads("selfref-mut", definition), "a §8 collection $mut referencing a field on a §5.8 self-ref collection loads");
}
