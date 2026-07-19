//! RED TEAM — §5.3 static structs × §5.8 self-referential collections, at COMPILE
//! time. A neighbour of the F1 landing (commit f252ed0): F1 fixed a self-ref-deep
//! collection's OWN computed/`$check` compile (`compile_collection` now falls back
//! to `Schema::collection_row_type`), but it did NOT fix the same depth-cap seam in
//! the STATIC-STRUCT compile path.
//!
//! # Finding (F-N1 — FIXED; these repros now run as regression guards)
//!
//! A static struct (§5.3) declared inside a self-referential collection (§5.8),
//! whose `$check` or computed value (§5.2/§5.10) references the struct's OWN member
//! (`.tag`), is **rejected at load** with `E-EXPR "no field \`tag\` on this row"`
//! (the `$types` self-ref spelling) or `E-EXPR "cannot compare \`json\` with
//! \`text\`"` (the `$like: "^"` spelling). The IDENTICAL struct+expression on a
//! NON-self-referential collection loads fine, and the self-ref collection with a
//! PLAIN struct (no struct-internal expression) also loads fine.
//!
//! ## Root cause
//!
//! `compile_collection` expands a self-referential shape to `MAX_SELF_REF_DEPTH =
//! 32` (compiled.rs:642) and compiles the struct member at EVERY depth via
//! `compile_struct` (compiled.rs:808). `compile_struct` builds the struct's lexical
//! scope from `struct_contexts` (compiled.rs:861-865), which resolves each ancestor
//! prefix through `Schema::receiver_row_type` (schema.rs:44-54). That resolver folds
//! the self-ref field to opaque `json` once its `MAX_DEPTH = 32` (schema.rs:17, ~2
//! units per self-ref level) is exceeded, then returns `None`; `struct_contexts`
//! `filter_map`s the `None` away, so at the deep compiled self-ref levels the
//! contexts chain is TRUNCATED. The struct's own `.` row type (`contexts.last()`,
//! compiled.rs:822) is then a shallow ANCESTOR row — a `company` row that has
//! `name`/`id`/`subcompanies` but no `tag` — so a struct expression reading `.tag`
//! fails to resolve and the whole load is rejected. (A `^.name`-only expression
//! survives by coincidence: the mis-bound `.` is already a `company` row, so its
//! `^` still finds `name` — every self-ref row shares one shape. Only a struct-OWN
//! member reference exposes the break.)
//!
//! F1's fix touched `compile_collection`'s `row_ty` fallback only; `struct_contexts`
//! / `compile_struct` were left on the truncating `receiver_row_type`. F-N1 fixes the
//! ROOT: the struct compile now builds its lexical chain from `Schema::context_chain`,
//! which descends the declaration path directly from shapes — a self-ref row shares
//! one shape at every depth, so the chain is complete and correctly aligned at every
//! depth (the same shape-based fallback F1 gave the collection row).
//!
//! ## Why it is a bug (not acknowledged debt)
//!
//! §5.2/§5.10 place no restriction on static structs inside self-referential
//! collections; §5.3 says a struct's fields/computed/checks resolve during the
//! containing insertion. The struct declares `tag` at every real depth (self-ref
//! rows are identically shaped); the truncated fallback is a compile-internal
//! artifact of the two independent depth caps not lining up, not a spec-mandated
//! rejection. The failure is fail-closed (a false LOAD rejection of a spec-valid
//! package), so it is a usability/correctness gap, not an integrity or authz breach.
//! The only documented struct/self-ref seam is a nested COLLECTION inside a struct
//! (compiled.rs:806/848) — the reverse shape, not this one.
//!
//! The `must_load` tests assert the SPEC-correct outcome (the package loads); with
//! F-N1 landed they pass. The controls fence the boundary so the fix cannot regress
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
// F-N1 REGRESSION GUARDS — assert the SPEC-correct outcome (the package loads).
// Was RED before F-N1; GREEN now.
// ===========================================================================

/// §5.3 + §5.8: a static struct inside a self-referential collection whose `$check`
/// reads its OWN member (`.tag != ''`) MUST load — `tag` is a real struct member at
/// every self-ref depth.
#[test]
fn selfref_struct_ownfield_check_must_load() {
    let definition = r##"{
      "$liasse": 1, "$app": "t.srsc@1.0.0",
      "$types": { "company": { "$key": "id", "id": "text", "name": "text",
        "meta": { "tag": "text", "$check": [".tag != ''", "tag required"] },
        "subcompanies": "company" } },
      "$model": { "companies": "company", "flat": { "$view": ".companies { id, name }" } }
    }"##;
    assert!(loads("selfref-struct-check", definition), "a §5.3 struct own-member $check on a §5.8 self-ref collection must load");
}

/// §5.2 + §5.8: a static struct computed value reading its OWN member (`echo: = .tag`)
/// MUST load on a self-referential collection.
#[test]
fn selfref_struct_ownfield_computed_must_load() {
    let definition = r##"{
      "$liasse": 1, "$app": "t.srscomp@1.0.0",
      "$types": { "company": { "$key": "id", "id": "text", "name": "text",
        "meta": { "tag": "text", "echo": "= .tag" },
        "subcompanies": "company" } },
      "$model": { "companies": "company", "flat": { "$view": ".companies { id, name }" } }
    }"##;
    assert!(loads("selfref-struct-computed", definition), "a §5.2 struct own-member computed on a §5.8 self-ref collection must load");
}

/// §6.2 + §5.8: a static struct `$check` reading the containing row via `^` AND its
/// own member MUST load on a self-referential collection. (`.tag == ^.name` reads
/// both `.tag` — the struct member that trips the truncation — and `^.name`.)
#[test]
fn selfref_struct_caret_check_must_load() {
    let definition = r##"{
      "$liasse": 1, "$app": "t.srscar@1.0.0",
      "$types": { "company": { "$key": "id", "id": "text", "name": "text",
        "meta": { "tag": "text", "$check": [".tag == ^.name", "tag must equal name"] },
        "subcompanies": "company" } },
      "$model": { "companies": "company", "flat": { "$view": ".companies { id, name }" } }
    }"##;
    assert!(loads("selfref-struct-caret", definition), "a §6.2 struct `^`+own-member $check on a §5.8 self-ref collection must load");
}

/// The same finding via the `$like: "^"` spelling of self-reference (§5.8). Here the
/// truncation surfaces as `E-EXPR "cannot compare \`json\` with \`text\`"` rather
/// than "no field", but the root cause is identical.
#[test]
fn selfref_like_struct_ownfield_check_must_load() {
    let definition = r##"{
      "$liasse": 1, "$app": "t.srlsc@1.0.0",
      "$model": { "companies": { "$key": "id", "id": "text", "name": "text",
        "meta": { "tag": "text", "$check": [".tag != ''", "tag required"] },
        "subcompanies": { "$like": "^" } },
        "flat": { "$view": ".companies { id, name }" } }
    }"##;
    assert!(loads("selfref-like-struct-check", definition), "a §5.3 struct own-member $check on a `$like: ^` self-ref collection must load");
}

// ===========================================================================
// PASSING CONTROLS — run by default; fence the boundary so the bug is SPECIFIC to
// (self-ref × struct-OWN-member expression), not a blanket break of struct exprs.
// ===========================================================================

/// CONTROL: the self-ref collection with a PLAIN struct (no struct-internal
/// expression) loads — self-ref × static struct itself is fine (§5.3/§5.8).
#[test]
fn control_selfref_plain_struct_loads() {
    let definition = r##"{
      "$liasse": 1, "$app": "t.srplain@1.0.0",
      "$types": { "company": { "$key": "id", "id": "text", "name": "text",
        "meta": { "tag": "text", "note": "text" },
        "subcompanies": "company" } },
      "$model": { "companies": "company", "flat": { "$view": ".companies { id, name }" } }
    }"##;
    assert!(loads("selfref-plain-struct", definition), "a §5.8 self-ref collection with a plain §5.3 struct loads");
}

/// CONTROL: the IDENTICAL struct+own-member `$check` on a NON-self-referential
/// collection loads — struct `$check`s themselves are fine (F2 domain). Isolates the
/// break to the self-ref depth-cap seam.
#[test]
fn control_nonselfref_struct_ownfield_check_loads() {
    let definition = r##"{
      "$liasse": 1, "$app": "t.nsc@1.0.0",
      "$model": {
        "people": { "$key": "id", "id": "text", "name": "text",
          "meta": { "tag": "text", "$check": [".tag != ''", "tag required"] } },
        "$mut": { "add": ".people + { id: @id, name: @name, meta: { tag: @tag } }" }
      }
    }"##;
    assert!(loads("nonselfref-struct-check", definition), "a §5.3 struct own-member $check on a non-self-ref collection loads");
}

/// CONTROL: a self-ref struct whose expression references ONLY the parent via `^`
/// (`= ^.name`, no own-member) loads — the mis-bound `.` is a self-ref row that also
/// carries `name`, so a `^`-only reference survives the truncation. This is the
/// asymmetry that proves the break is specifically the struct-OWN member reference.
#[test]
fn control_selfref_struct_caret_only_computed_loads() {
    let definition = r##"{
      "$liasse": 1, "$app": "t.srcar@1.0.0",
      "$types": { "company": { "$key": "id", "id": "text", "name": "text",
        "meta": { "tag": "text", "parent_name": "= ^.name" },
        "subcompanies": "company" } },
      "$model": { "companies": "company", "flat": { "$view": ".companies { id, name }" } }
    }"##;
    assert!(loads("selfref-struct-caret-only", definition), "a §6.2 struct `^`-only computed on a §5.8 self-ref collection loads");
}

/// CONTROL: the same `$types` `company` shape (struct + own-member `$check`) used
/// WITHOUT the `subcompanies` self-ref field loads — proving the trigger is the
/// self-referential expansion, not the `$types` indirection.
#[test]
fn control_types_struct_check_without_selfref_loads() {
    let definition = r##"{
      "$liasse": 1, "$app": "t.nsr@1.0.0",
      "$types": { "company": { "$key": "id", "id": "text", "name": "text",
        "meta": { "tag": "text", "$check": [".tag != ''", "tag required"] } } },
      "$model": { "companies": "company", "flat": { "$view": ".companies { id, name }" } }
    }"##;
    assert!(loads("types-struct-no-selfref", definition), "the same `$types` struct+check without the self-ref field loads");
}
