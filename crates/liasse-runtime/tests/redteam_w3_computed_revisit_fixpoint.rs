#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
#![allow(clippy::doc_lazy_continuation)]
//! RED-TEAM (WAVE 3) — §5.2 cross-collection computed fixpoint UNDER-ITERATES.
//!
//! The wave-2 fix (commit 0d9427f, `eval.rs::expose_computed`) iterates the
//! collection-computed fold to a cross-collection fixed point, but bounds the
//! number of passes by the number of COLLECTIONS carrying computed values:
//!
//!   let bound = self.compiled.collections.iter().filter(|c| !c.computed.is_empty()).count();
//!   for _ in 0..bound { ... one pass ... }
//!
//! and its own doc claims "the number of collections carrying computed values
//! bounds the longest cross-collection chain (one pass advances the resolved
//! frontier by one hop)."
//!
//! That claim is FALSE. Each pass advances the resolved frontier by exactly one
//! cross-collection HOP (the env is rebuilt only BETWEEN passes), so the number of
//! passes needed equals the number of computed->computed edges on the LONGEST
//! dependency path. A path can traverse the SAME collection twice through two
//! different computed fields, so its hop-count can EXCEED the number of collections.
//!
//! §5.2 (verbatim): "A computed value is read-only and determined by its
//! expression. It participates in views, checks, sorting, and projections **like
//! any other value**. ... A computed expression yielding `none` produces an absent
//! optional value."
//!
//! And §5.1: "Defaults and computed insertion values form one dependency graph. The
//! model is valid when that graph is acyclic, and the implementation MAY evaluate
//! it in any topological order." So an acyclic computed graph MUST fully resolve;
//! a value is absent ONLY when its own expression yields `none`, never because the
//! engine stopped iterating early.
//!
//! The tell-tale: `liasse-model/src/infer.rs::computed_types` bounds its own type
//! inference by `pending_count` — the number of computed FIELDS (here 3) — and so
//! it fully TYPES the field below as `int`. The runtime then bounds by collections
//! (here 2) and DROPS it. The type system promises `a.f1: int`; the runtime hands
//! back an absent value. That divergence is the bug.
//!
//! THE SHAPE (near-cyclic but strictly acyclic, §5.1):
//!   collection `a`, row "main":  base = 10 (stored)
//!                                f2   = .base * 2            (same-row computed leaf)
//!                                f1   = /b["only"].g + 1     (reads b's computed)
//!   collection `b`, row "only":  g    = /a["main"].f2 + 100  (reads a's OTHER computed)
//!
//! There is no cycle: f1 depends on g, g depends on f2, f2 depends on the stored
//! base. But the path a.f1 -> b.g -> a.f2 -> (stored) visits collection `a` twice.
//! Resolution order (each cross-collection hop is one pass):
//!   pass 1: a.f2 = 20 (per-row leaf)
//!   pass 2: b.g  = a.f2 + 100 = 120
//!   pass 3: a.f1 = b.g  + 1   = 121
//! Three passes are required, but `bound` = 2 (collections `a` and `b`), so the
//! loop stops after pass 2 and `a.f1` is left ABSENT — a §5.2 value silently
//! dropped, though its expression yields 121, not `none`.
//!
//! Every expected value is arithmetic derivable from SPEC.md and `$data` alone
//! (base=10 -> f2=20 -> g=120 -> f1=121), never from observed behaviour.
//!
//! Root cause: `crates/liasse-runtime/src/eval.rs::expose_computed` (~L236) — the
//! pass bound counts collections, not the longest computed->computed chain length;
//! a collection revisited via two computed fields needs one pass more than there
//! are collections.

mod support;

use liasse_runtime::{Engine, Value, ViewResult};
use liasse_store::MemoryStore;
use liasse_value::Integer;
use support::store;

/// An `int` view field value.
fn int(n: i64) -> Value {
    Value::Int(Integer::from(n))
}

fn load(instance: &str, definition: &str) -> Engine<MemoryStore> {
    let mut generators = support::generator();
    match Engine::load(store(instance), definition, &mut generators) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error}"),
    }
}

/// The single-row view result, by view name.
fn one_row_view(engine: &Engine<MemoryStore>, view: &str) -> ViewResult {
    let result = engine.view_at_head(view).expect("view resolves").expect("view declared");
    assert_eq!(result.rows().len(), 1, "{view}: exactly one row");
    result
}

// ── THE FINDING ──────────────────────────────────────────────────────────────
// Collection `a` is revisited by the dependency path (a.f1 -> b.g -> a.f2), so the
// chain needs 3 passes while `bound` = 2 (two collections). `a.f1` is dropped.
#[test]
fn cross_collection_revisit_drops_deepest_computed() {
    let definition = r#"{
  "$liasse": 1,
  "$app": "t.w3xcc.revisit@1.0.0",
  "$model": {
    "a": {
      "$key": "k",
      "k": "text",
      "base": "int",
      "f2": "= .base * 2",
      "f1": "= /b[\"only\"].g + 1"
    },
    "b": {
      "$key": "k",
      "k": "text",
      "g": "= /a[\"main\"].f2 + 100"
    },
    "av": { "$view": ".a { k, f2, f1 }" },
    "bv": { "$view": ".b { k, g }" }
  },
  "$data": {
    "a": { "main": { "base": "10" } },
    "b": { "only": {} }
  }
}"#;
    let engine = load("w3-xcc-revisit", definition);

    // b.g reads a.f2 (one hop): resolves within the collection budget -> 120.
    let bv = one_row_view(&engine, "bv");
    assert_eq!(bv.rows()[0].field("g"), Some(&int(120)), "b.g = a.f2 + 100 = 120 (§5.2)");

    // a.f2 is a same-row leaf: resolves in pass 1 -> 20.
    let av = one_row_view(&engine, "av");
    let arow = &av.rows()[0];
    assert_eq!(arow.field("f2"), Some(&int(20)), "a.f2 = base * 2 = 20 (§5.2)");

    // a.f1 reads b.g, which reads a.f2: the third hop. §5.2 requires it present and
    // equal to 121 (its expression yields 121, never `none`). The fix stops after
    // `bound` = 2 passes, leaving it ABSENT.
    assert!(
        arow.field("f1").is_some(),
        "§5.2: a computed value participates like any other value and is absent ONLY \
         when its expression yields `none`; a.f1 = /b.g + 1 = 121 yields a value, yet the \
         cross-collection fold stopped after `bound` (= 2 collections) passes — one short \
         of the 3-hop chain a.f1 -> b.g -> a.f2 — and dropped it",
    );
    assert_eq!(
        arow.field("f1"),
        Some(&int(121)),
        "§5.2: a.f1 = /b[\"only\"].g + 1 = 120 + 1 = 121",
    );
}

// ── CONTROL: same near-cyclic shape, but the revisited field is STORED ─────────
// With `a.f2` a stored field, b.g resolves in pass 1 (reads a stored value), so the
// chain is only 2 hops and `bound` = 2 suffices. This isolates the defect exactly:
// the ONLY change from the finding is making the revisited link a stored field
// instead of a computed one, which removes the extra hop.
#[test]
fn control_revisit_with_stored_intermediate_resolves() {
    let definition = r#"{
  "$liasse": 1,
  "$app": "t.w3xcc.storedmid@1.0.0",
  "$model": {
    "a": {
      "$key": "k",
      "k": "text",
      "f2": "int",
      "f1": "= /b[\"only\"].g + 1"
    },
    "b": {
      "$key": "k",
      "k": "text",
      "g": "= /a[\"main\"].f2 + 100"
    },
    "av": { "$view": ".a { k, f2, f1 }" },
    "bv": { "$view": ".b { k, g }" }
  },
  "$data": {
    "a": { "main": { "f2": "20" } },
    "b": { "only": {} }
  }
}"#;
    let engine = load("w3-xcc-stored", definition);

    let bv = one_row_view(&engine, "bv");
    assert_eq!(bv.rows()[0].field("g"), Some(&int(120)), "b.g = a.f2(stored) + 100 = 120");

    let av = one_row_view(&engine, "av");
    assert_eq!(
        av.rows()[0].field("f1"),
        Some(&int(121)),
        "control: stored intermediate -> 2-hop chain -> resolves within `bound` = 2",
    );
}

// ── CONTROL (DRY proof): straight 4-deep chain across 4 DISTINCT collections ───
// d.cd(leaf) -> c.cc -> b.cb -> a.ca is a 4-level chain over 4 collections, so
// `bound` = 4 passes exactly cover it. This proves cross-collection depth per se is
// handled by the fix; only a chain whose hop-count EXCEEDS the collection count
// (via a revisited collection, above) is dropped.
#[test]
fn control_straight_four_deep_chain_resolves() {
    let definition = r#"{
  "$liasse": 1,
  "$app": "t.w3xcc.fourdeep@1.0.0",
  "$model": {
    "d": { "$key": "k", "k": "text", "base": "int", "cd": "= .base * 2" },
    "c": { "$key": "k", "k": "text", "cc": "= /d[\"m\"].cd + 1" },
    "b": { "$key": "k", "k": "text", "cb": "= /c[\"m\"].cc + 1" },
    "a": { "$key": "k", "k": "text", "ca": "= /b[\"m\"].cb + 1" },
    "av": { "$view": ".a { k, ca }" }
  },
  "$data": {
    "d": { "m": { "base": "10" } },
    "c": { "m": {} },
    "b": { "m": {} },
    "a": { "m": {} }
  }
}"#;
    let engine = load("w3-xcc-fourdeep", definition);
    let av = one_row_view(&engine, "av");
    // cd = 20, cc = 21, cb = 22, ca = 23.
    assert_eq!(
        av.rows()[0].field("ca"),
        Some(&int(23)),
        "a straight 4-deep chain over 4 collections resolves in `bound` = 4 passes",
    );
}
