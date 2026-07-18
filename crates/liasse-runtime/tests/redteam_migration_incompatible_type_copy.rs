#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED-TEAM probe (Target A §20 migration validation × §5.1 typed state × §19.10
//! round-trip): a §20 migration that changes a field's scalar TYPE with no
//! `$from`/`$as` transform COPIES THE OLD VALUE VERBATIM, committing state whose
//! stored value is NOT of the field's declared type.
//!
//! # DEFECT
//!
//! §20.1 pins the migration order as "compatible same-identity copy, local `$from`
//! mappings ..., then the selected package-level statements", and states: **"Without
//! `$as`, the *compatible* value is copied."** A field whose declared scalar type
//! changed (`decimal` -> `int`, `text` -> `int`) is a BREAKING change (Annex E:
//! breaking changes use a new major), so its old value is NOT a compatible value —
//! there is nothing for the copy step to copy. Such a field either needs an `$as`
//! transform producing the new type, or (with none) the migration must reject it,
//! exactly as `coerce_and_require` already rejects a required field a migration
//! left unpopulated (migrate.rs:463). §20.1 further requires the complete
//! prospective target to be "checked under ordinary keys, refs, uniqueness, checks,
//! ..." — and an ordinary admission never commits a value that is not of the
//! field's declared type (§5.1: a field holds a typed value).
//!
//! The engine does neither. `map_row` (crates/liasse-runtime/src/migrate.rs:709)
//! copies the source value into the target field BY NAME, regardless of whether the
//! declared type changed:
//!
//! ```ignore
//! } else if let Some(value) = old_row.get(&field.name) {
//!     fields.insert(field.name.clone(), value.clone()); // §20.1 "compatible copy"
//! }
//! ```
//!
//! and the §20.1 final pass `coerce_and_require`
//! (crates/liasse-runtime/src/migrate.rs:419) re-decodes ONLY ref-typed and
//! enum-bearing fields — a plain scalar field of a changed base type is neither, so
//! it is never re-validated against the target type. `rules::finalize` then runs
//! `$check`s / refs / uniqueness / buckets, none of which re-decode a scalar
//! against its declared type (crates/liasse-runtime/src/rules.rs:331). The
//! wrong-typed `Value` is staged into the store verbatim (`MemoryStore` keeps the
//! `Value`; there is no decode-on-read), so committed live state holds, e.g., a
//! `Value::Decimal("1.50")` or a `Value::Text("hello")` in a field the active model
//! declares `int`.
//!
//! # WHY IT IS A CONCRETE VIOLATION (not "author beware in a MayBreak major")
//!
//! Every input here was legitimately admitted: `1.50` was a valid `decimal` and
//! `hello` a valid `text` under v1. §20.3 permits a breaking field-type change at a
//! new major, but §20.1 still requires the migrated result to be the *compatible*
//! copy and to pass ordinary validation. The committed result is provably
//! type-INVALID, demonstrated two independent ways below:
//!
//!   * `migration_retypes_field_must_not_commit_wrong_type`: after the migration
//!     commits, the field the model now declares `int` reads back a `Decimal`.
//!   * `migrated_wrong_typed_state_breaks_export_restore`: exporting the migrated
//!     boundary and restoring it FAILS — the §19.5/§19.7 portable codec, which DOES
//!     decode each field against its declared type, rejects `int value hello`. So
//!     the migration produced a boundary that violates §19.10 (a restored boundary
//!     reproduces the owned logical state): it cannot be restored at all.
//!
//! # ISOLATION
//!
//! `control_compatible_major_preserves_and_round_trips` runs the SAME major-bump
//! migration machinery over a field whose type is UNCHANGED (a real compatible
//! copy) and adds an unrelated field: the value is preserved and the boundary
//! export/restores cleanly. So the migration + export machinery works; the defect
//! is specific to copying an incompatible-typed value. The expected values are
//! externally deducible from SPEC.md text alone (§20.1 "compatible value", §5.1
//! typed state, §19.10 round-trip); nothing here encodes implementation behavior.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, UpdateError, Value};
use liasse_store::MemoryStore;
use liasse_value::{Decimal, Text};
use support::{generator, store};

fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}

fn dec(v: &str) -> Value {
    Value::Decimal(Decimal::parse(v).expect("decimal parses"))
}

fn commit(o: CallOutcome) {
    assert!(matches!(o, CallOutcome::Committed { .. }), "expected a commit, got {o:?}");
}

/// The single `total` cell of the `all` view (the migrated field under test).
fn total(engine: &Engine<MemoryStore>) -> Option<Value> {
    let view = engine.view_at_head("all").expect("view ok").expect("all declared");
    view.rows().first().and_then(|row| row.field("total").cloned())
}

/// v1: `orders.total` is a `decimal`; the `all` view exposes it.
const V1_DECIMAL: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.retype@1.0.0",
  "$model": {
    "orders": { "$key": "id", "id": "text", "total": "decimal" },
    "all": { "$view": ".orders { id, total }" },
    "$mut": { "add": ".orders + { id: @id, total: @total }" }
  }
}"#;

/// v2 (MAJOR bump 2.0.0): `orders.total` is retyped `decimal` -> `int` with NO
/// `$from`/`$as` transform. A breaking type change belongs at a new major (§20.3);
/// the old `decimal` value is NOT a compatible value for the `int` field.
const V2_INT: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.retype@2.0.0",
  "$model": {
    "orders": { "$key": "id", "id": "text", "total": "int" },
    "all": { "$view": ".orders { id, total }" },
    "$mut": { "add": ".orders + { id: @id, total: @total }" }
  }
}"#;

/// v1: `orders.total` is a `text`.
const V1_TEXT: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.retypetext@1.0.0",
  "$model": {
    "orders": { "$key": "id", "id": "text", "total": "text" },
    "all": { "$view": ".orders { id, total }" },
    "$mut": { "add": ".orders + { id: @id, total: @total }" }
  }
}"#;

/// v2 (MAJOR bump): `total` retyped `text` -> `int`, NO transform. The stored text
/// `"hello"` is not even parseable as an `int`, so committed state is unambiguously
/// type-invalid.
const V2_TEXT_TO_INT: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.retypetext@2.0.0",
  "$model": {
    "orders": { "$key": "id", "id": "text", "total": "int" },
    "all": { "$view": ".orders { id, total }" },
    "$mut": { "add": ".orders + { id: @id, total: @total }" }
  }
}"#;

/// v2 CONTROL (MAJOR bump): `total` type UNCHANGED (`decimal`), an unrelated field
/// `note` added. This exercises the identical migration path over a genuinely
/// compatible field, so the copy is legitimate.
const V2_COMPAT: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.retype@2.0.0",
  "$model": {
    "orders": { "$key": "id", "id": "text", "total": "decimal", "note": "text = 'n'" },
    "all": { "$view": ".orders { id, total }" },
    "$mut": { "add": ".orders + { id: @id, total: @total }" }
  }
}"#;

/// §20.1/§5.1: after a migration that RETYPES `total` to `int` commits, the stored
/// value MUST be an `int` (a compatible copy has nothing to copy for an
/// incompatible type change, so the migration must instead reject or transform).
/// The engine commits the source `decimal` verbatim — a value of the wrong type in
/// committed live state.
#[test]
fn migration_retypes_field_must_not_commit_wrong_type() {
    let mut engine = Engine::load(store("mig-retype-dec"), V1_DECIMAL, &mut generator()).expect("v1 loads");
    let mut g = generator();
    commit(engine.call(&CallRequest::new("add").arg("id", text("o1")).arg("total", dec("1.50")), &mut g).expect("add"));
    assert!(matches!(total(&engine), Some(Value::Decimal(_))), "pre-migration `total` is a decimal");

    match engine.update(V2_INT, &mut generator()) {
        // Spec-acceptable outcome A: reject the migration — there is no compatible
        // `int` value to copy from the old `decimal` and no `$as` was declared.
        Err(UpdateError::Rejected(_)) => {}
        // Spec-acceptable outcome B: commit, but only with an `int`-typed value.
        Ok(_) => {
            let stored = total(&engine).expect("migrated `total` present");
            assert!(
                matches!(stored, Value::Int(_)),
                "§20.1/§5.1: a migration retyping `orders.total` to `int` committed a NON-int value \
                 into it — the source `decimal` was copied verbatim by `map_row` (migrate.rs:709) and \
                 `coerce_and_require` re-validates only ref/enum fields, so a scalar type change is \
                 never re-typed. Committed live state now holds {:?} in an `int` field (violates §5.1 \
                 typed state; §20.1 'the compatible value is copied' has no compatible value here).",
                stored.to_wire(),
            );
        }
        Err(other) => panic!("migration failed unexpectedly: {other}"),
    }
}

/// §19.10/§20.1: the migrated boundary must round-trip through export/restore. When
/// the migration retypes `total` from `text` to `int` and copies `"hello"`
/// verbatim, the committed state is un-restorable: the §19 portable codec decodes
/// each field against its declared type and rejects `int value hello`. The migration
/// therefore produced a boundary that cannot reproduce its own owned state (§19.10).
#[test]
fn migrated_wrong_typed_state_breaks_export_restore() {
    let mut engine = Engine::load(store("mig-retype-txt"), V1_TEXT, &mut generator()).expect("v1 loads");
    let mut g = generator();
    commit(engine.call(&CallRequest::new("add").arg("id", text("o1")).arg("total", text("hello")), &mut g).expect("add"));

    let outcome = engine.update(V2_TEXT_TO_INT, &mut generator());
    if matches!(outcome, Err(UpdateError::Rejected(_))) {
        // Spec-acceptable: the migration refused the incompatible copy. Nothing to
        // round-trip; the defect did not occur.
        return;
    }
    assert!(outcome.is_ok(), "migration failed unexpectedly: {outcome:?}");

    // The migration committed. §19.10 REQUIRES the exported boundary to restore.
    let artifact = engine.export().expect("export of the migrated boundary");
    let restored = Engine::restore(store("mig-retype-txt"), &artifact, &mut generator());
    assert!(
        restored.is_ok(),
        "§19.10/§20.1/§5.1: the migration retyped `orders.total` to `int` and copied the source \
         `text \"hello\"` verbatim, so exporting the migrated boundary and restoring it FAILS \
         ({}) — the portable codec enforces the declared `int` type that live state violates. A \
         migration must not commit a value that is not of the field's declared type.",
        match &restored {
            Err(e) => e.to_string(),
            Ok(_) => "unexpectedly ok".to_owned(),
        },
    );
}

/// GENERALIZATION 1 — the same hole applies when a transform IS present but its
/// RESULT type differs from the target field. `$as: "."` (identity) over the old
/// `decimal` returns a `decimal` for the `int` field. `migrate.rs`'s `transform`
/// type-checks the expression against `dot_ty` (the OLD type) but never constrains
/// the result to the target field type, and no later pass re-validates it — so the
/// gap is not limited to the no-`$as` verbatim copy: NO migrated scalar value is
/// type-checked against its declared field type.
const V2_INT_AS_IDENTITY: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.retype@2.0.0",
  "$model": {
    "orders": { "$key": "id", "id": "text", "total": { "$type": "int", "$from": "total", "$as": "." } },
    "all": { "$view": ".orders { id, total }" },
    "$mut": { "add": ".orders + { id: @id, total: @total }" }
  }
}"#;

#[test]
fn migration_as_transform_must_produce_target_type() {
    let mut engine = Engine::load(store("mig-as-ident"), V1_DECIMAL, &mut generator()).expect("v1 loads");
    let mut g = generator();
    commit(engine.call(&CallRequest::new("add").arg("id", text("o1")).arg("total", dec("2.5")), &mut g).expect("add"));

    match engine.update(V2_INT_AS_IDENTITY, &mut generator()) {
        Err(UpdateError::Rejected(_)) => {}
        Ok(_) => {
            let stored = total(&engine).expect("migrated `total` present");
            assert!(
                matches!(stored, Value::Int(_)),
                "§20.1/§5.1: an `$as` transform result is not type-checked against the target field: \
                 `$as: \".\"` returned a `decimal` for an `int` field and committed {:?} — the migration \
                 must produce a value of the field's declared type (a wrong-typed `$as` result must reject).",
                stored.to_wire(),
            );
        }
        Err(other) => panic!("migration failed unexpectedly: {other}"),
    }
}

/// GENERALIZATION 2 — the hole also covers §8.2 root SINGLETON members. The
/// singleton compatible-copy (migrate.rs:296-309) carries each old member value by
/// name with no type re-validation, so a root scalar retyped `decimal` -> `int`
/// lands verbatim and the migrated boundary cannot be restored (§19.10). This is a
/// distinct code path from the collection-field copy above.
const S_V1: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.singretype@1.0.0",
  "$model": {
    "flag": "decimal",
    "readout": { "$view": ". { flag }" },
    "$mut": { "set_flag": ".flag = @v" }
  },
  "$data": { "flag": "1.5" }
}"#;

const S_V2: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.singretype@2.0.0",
  "$model": {
    "flag": "int",
    "readout": { "$view": ". { flag }" },
    "$mut": { "set_flag": ".flag = @v" }
  },
  "$data": { "flag": "0" }
}"#;

#[test]
fn migration_retypes_singleton_member_breaks_restore() {
    let mut engine = Engine::load(store("mig-sing-retype"), S_V1, &mut generator()).expect("v1 loads");
    let flag = |e: &Engine<MemoryStore>| -> Option<Value> {
        e.view_at_head("readout").expect("view ok").expect("readout declared").rows().first().and_then(|r| r.field("flag").cloned())
    };
    assert!(matches!(flag(&engine), Some(Value::Decimal(_))), "pre-migration singleton `flag` is a decimal");

    let outcome = engine.update(S_V2, &mut generator());
    if matches!(outcome, Err(UpdateError::Rejected(_))) {
        return; // spec-acceptable: rejected the incompatible singleton retype.
    }
    assert!(outcome.is_ok(), "migration failed unexpectedly: {outcome:?}");

    let artifact = engine.export().expect("export of the migrated boundary");
    let restored = Engine::restore(store("mig-sing-retype"), &artifact, &mut generator());
    assert!(
        restored.is_ok(),
        "§19.10/§20.1/§8.2/§5.1: a §8.2 root singleton member retyped `decimal`->`int` was carried \
         verbatim, so the migrated boundary does not restore ({}). The §8.2 singleton compatible copy \
         (migrate.rs:296-309) never re-validates a member against the target type.",
        match &restored { Err(e) => e.to_string(), Ok(_) => "unexpectedly ok".to_owned() },
    );
}

/// CONTROL (must pass): the SAME major-bump migration machinery over a field whose
/// type is UNCHANGED — a legitimate §20.1 compatible copy — preserves the value and
/// the boundary export/restores cleanly. Isolates the defect to the incompatible
/// retype above.
#[test]
fn control_compatible_major_preserves_and_round_trips() {
    let mut engine = Engine::load(store("mig-retype-ctrl"), V1_DECIMAL, &mut generator()).expect("v1 loads");
    let mut g = generator();
    commit(engine.call(&CallRequest::new("add").arg("id", text("o1")).arg("total", dec("1.50")), &mut g).expect("add"));

    engine.update(V2_COMPAT, &mut generator()).expect("compatible major update commits");
    assert_eq!(total(&engine), Some(dec("1.50")), "§20.1: an unchanged-type field is copied verbatim and preserved");

    let artifact = engine.export().expect("export");
    let restored = Engine::restore(store("mig-retype-ctrl"), &artifact, &mut generator()).expect("restore ok");
    assert_eq!(total(&restored), Some(dec("1.50")), "§19.10: the compatible migrated boundary round-trips");
}
