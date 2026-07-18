#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team regression: the Annex E boundary-narrowing check compares a view's
//! exposed **row identity** as a typed tuple of `(name, Type)` per `$key`
//! component — BUT it reads each component's `Type` from the collection's *scalar*
//! `field(name)` descriptor. A struct-typed `$key` (A.8: "structs composed solely
//! of key-eligible required fields") is NOT a scalar field: the runtime compiles a
//! `Node::Struct` member into `CompiledCollection.structs`, not `.fields`, so
//! `CompiledCollection::field(name)` returns `None` and `exposed_identity` falls
//! back to `Type::Json` for that whole component. Both the active and candidate
//! contracts therefore record the struct key component as `json`, so retyping an
//! INNER struct member (`y: int` → `y: text`) leaves the compared identity byte-for-
//! byte identical (`[("loc", json)]` vs `[("loc", json)]`) and the check reports no
//! narrowing. The exposed row identity nonetheless changed — A.9 makes the key a
//! typed tuple, and the struct's member types are part of that tuple's type — so a
//! same-major minor that changes it is breaking under E.5 and MUST be rejected.
//!
//! Spec chain (all normative; Annex E is normative):
//!   * A.8 (SPEC.md line 4472): a `$key` MAY be a "struct composed solely of
//!     key-eligible required fields". Both `{x:int,y:int}` and `{x:int,y:text}` are
//!     such structs, so both releases load — the retype is not caught at build.
//!   * A.9 (line 4479): a key is a *typed tuple* ("the exact key type of its target
//!     collection"). A struct key's type is `struct{x, y}`; changing member `y`'s
//!     type changes that key type — the exposed identity's type.
//!   * E.3 (line 5192): mechanically decidable checks include "row identity ..."
//!     and "types ..." — identity IS mechanically compared and types are what the
//!     comparison is made of. E.3: the checker "MUST reject every narrowing it can
//!     establish from those contracts"; a struct member retype is mechanically
//!     visible (both shapes are fully declared).
//!   * E.5 (line 5236): breaking output changes include "changing exposed row
//!     identity". Retyping `loc.y` from `int` to `text` changes it.
//!   * §20.3 / E.9 (line 5163, 5281): `load` and update reject a narrowing release
//!     before activation; on rejection the current package, bindings, and state
//!     stay active.
//!
//! The collection is **empty** (no `$data`), so the `int`→`text` inner retype
//! passes prospective-state validation trivially (no stored struct-keyed row to
//! invalidate) and the ONLY thing that can reject the update is the Annex E
//! narrowing check. If that check misses it, the minor COMMITS — the bug.
//!
//! This is the struct-key analogue of the scalar / composite-of-scalars identity
//! holes pinned (and since fixed) in `redteam_compat_composite_identity`: those
//! carried the key component types once `field(name)` resolved them, but a struct
//! key never resolves through `field(name)` at all, so the type collapse to `json`
//! reopens exactly the same identity-type blind spot for struct keys.
//!
//! Root cause: `liasse-runtime/src/contract/mod.rs`, `exposed_identity`:
//! `collection.field(name).map_or(Type::Json, |field| field.ty.clone())`. For a
//! struct `$key`, `field(name)` is `None` (the struct is in `CompiledCollection.
//! structs`, populated at `compiled.rs` ~line 631, not in `.fields`), so the
//! component type is discarded and both releases compare as `json`.

mod support;

use liasse_runtime::{Engine, RejectionReason, UpdateError};
use liasse_store::MemoryStore;
use support::{generator, load};

/// Attempt an update, returning the raw result so a test can distinguish "the
/// minor committed" (the bug) from "rejected for a compatibility narrowing" (the
/// spec-required outcome) from "rejected for some other reason".
fn try_update(engine: &mut Engine<MemoryStore>, target: &str) -> Result<(), UpdateError> {
    let mut generator = generator();
    engine.update(target, &mut generator).map(|_| ())
}

/// Assert an update is refused specifically as an Annex E boundary-contract
/// narrowing (E.9) — not merely rejected for some incidental migration reason.
fn assert_narrowing(engine: &mut Engine<MemoryStore>, target: &str) -> String {
    match try_update(engine, target) {
        Err(UpdateError::Rejected(rejection)) if rejection.reason() == RejectionReason::Compatibility => {
            rejection.message().to_owned()
        }
        other => panic!("expected an Annex E compatibility narrowing rejection, got {other:?}"),
    }
}

// v1: a keyed collection whose `$key` is a **struct** `loc = { x: int, y: int }`
// (A.8 struct key). The public view projects only the NON-key field `value`, so the
// struct identity `loc` is exposed as the row identity (§7.2, inherited) but is not
// itself an output member — the identity path is the only thing that could catch a
// later inner-component retype. Empty (no `$data`), so no stored row constrains it.
const V1: &str = r#"{
  "$liasse": 1
  "$app": "t.compat.structid@1.0.0"
  "$model": {
    "cells": {
      "$key": "loc"
      "loc": { "x": "int", "y": "int" }
      "value": "text"
    }
    "$public": { "cells": { "$view": ".cells { value }" } }
  }
}"#;

/// THE BUG (A.8, A.9, E.3, E.5, §20.3). A minor retypes the INNER struct-key member
/// `loc.y` from `int` to `text`. The exposed row identity's typed tuple changes from
/// `struct{ x: int, y: int }` to `struct{ x: int, y: text }` — a breaking identity
/// change E.3 mechanically compares and E.5 forbids in a same-major forward release.
/// With the component unprojected and the collection empty, only the Annex E
/// narrowing check can reject it. It currently COMMITS: `exposed_identity` records
/// the struct key component as `json` (the struct is not a scalar `field`), so both
/// releases compare identity-equal.
#[test]
fn minor_retypes_struct_key_member_must_reject() {
    let mut engine = load("structid", V1);
    let target = V1
        .replace("@1.0.0", "@1.1.0")
        .replace(r#""y": "int""#, r#""y": "text""#);
    // Spec-required: an Annex E compatibility narrowing rejection (E.5 exposed row
    // identity). If the engine instead commits (or the model even declines to
    // build), this panics — reproducing the struct-identity-TYPE hole.
    let message = assert_narrowing(&mut engine, &target);
    assert!(
        message.to_lowercase().contains("identity"),
        "the diagnostic must report an exposed-row-identity narrowing (E.5), got: {message}"
    );
    // E.9: on rejection the prior release stays active and still promises the
    // `struct{ x: int, y: int }` identity.
    assert_eq!(engine.model().header().identity.version.minor, 0, "1.0.0 stays active");
}

// ---------------------------------------------------------------------------
// Controls: the SAME checker catches the retype when the struct key IS projected
// (output-member typing) and catches a struct-key NAME change (identity path).
// Together they prove the acceptance above is a struct-identity-TYPE hole, not a
// mis-derived rule or a wholesale blind spot for struct-keyed views.
// ---------------------------------------------------------------------------

// v1 variant whose public view DOES project the struct key `loc`, so the inner
// retype is visible to output-member typing.
const V1_PROJECTED: &str = r#"{
  "$liasse": 1
  "$app": "t.compat.structidp@1.0.0"
  "$model": {
    "cells": {
      "$key": "loc"
      "loc": { "x": "int", "y": "int" }
      "value": "text"
    }
    "$public": { "cells": { "$view": ".cells { loc, value }" } }
  }
}"#;

/// Control: the identical `loc.y: int → text` retype, but now `loc` is a projected
/// OUTPUT MEMBER. The projected member type comes from the typed view expression
/// (not from `exposed_identity`), so a changed struct member type is a changed
/// output member value type and the checker rejects it via the output-member path.
/// This proves the checker is not blind to the struct component's type in general —
/// only along the identity path, where the type is discarded.
#[test]
fn control_projected_struct_key_retype_rejected() {
    let mut engine = load("structidp", V1_PROJECTED);
    let target = V1_PROJECTED
        .replace("@1.0.0", "@1.1.0")
        .replace(r#""y": "int""#, r#""y": "text""#);
    // A narrowing rejection is required; the projected path names the output member.
    let _message = assert_narrowing(&mut engine, &target);
}

/// Control: a struct-key field NAME change `loc` → `pos` (view still projecting only
/// `value`) IS caught through the identity path, because the component name vectors
/// differ (`["loc"]` vs `["pos"]`). This proves the identity comparison is live for
/// struct-keyed collections — it simply discards the component's struct TYPE.
#[test]
fn control_struct_key_name_change_rejected() {
    let mut engine = load("structidn", V1);
    let target = V1
        .replace("@1.0.0", "@1.1.0")
        .replace(r#""$key": "loc""#, r#""$key": "pos""#)
        .replace(r#""loc": { "x": "int", "y": "int" }"#, r#""pos": { "x": "int", "y": "int" }"#);
    let message = assert_narrowing(&mut engine, &target);
    assert!(
        message.to_lowercase().contains("identity"),
        "a struct-key name change is an identity narrowing (E.5): {message}"
    );
}

/// Control (positive): a byte-identical struct-keyed model under a minor bump
/// commits. Proves the harness is sound and the checker does not reject a struct-
/// keyed view with an unprojected key wholesale — so the acceptance in the bug test
/// is specifically the missing identity-type comparison, not a dead code path.
#[test]
fn control_unchanged_struct_identity_commits() {
    let mut engine = load("structidok", V1);
    let target = V1.replace("@1.0.0", "@1.1.0");
    try_update(&mut engine, &target).expect("an unchanged struct-keyed minor commits");
    assert_eq!(engine.model().header().identity.version.minor, 1, "1.1.0 is active");
}
