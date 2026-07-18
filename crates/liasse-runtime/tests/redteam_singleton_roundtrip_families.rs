#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED-TEAM follow-on to Wave-17 Fix 3 (commit dfdd169, `StateSection` /
//! `singleton::row_type`, crates/liasse-runtime/src/portable.rs +
//! crates/liasse-runtime/src/singleton.rs). Fix 3 made a `.liasse` export capture
//! and restore the §8.2 package-root singleton reserved row. Its landed test
//! (`redteam_export_restore_root_singleton`) covered a scalar `flag` and one seeded
//! nested-struct member across export/restore. This file probes the harder edges
//! the task flagged, and confirms every one is handled:
//!
//!   * a singleton OPTIONAL field holding `none` round-trips to `none` (not dropped);
//!   * a nested static struct holding a scalar `$ref` round-trips;
//!   * a `$set`-of-`$ref` singleton, both at the root and inside a nested struct,
//!     round-trips its full membership;
//!   * `$normalize`/`$check` on a singleton field survive the round-trip AND stay
//!     enforced on the restored instance;
//!   * ROLLBACK (§19.8) restores the earlier singleton value;
//!   * a §19.9 MERGE combines a singleton change with a disjoint collection change
//!     cleanly, and reports a conflict when both sides change the same singleton;
//!   * a collections-only package (no singleton) still round-trips.
//!
//! Every expectation is deducible from SPEC.md text (§8.2 singleton state, §19.8
//! rollback, §19.9 merge, §19.10 restore reproduces owned logical state). All
//! assertions PASS at HEAD `2bae775`: convergence evidence that Fix 3 generalizes.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, ConflictCoordinate, Engine, ImportRelation, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, store};

fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}
fn commit(o: CallOutcome) {
    assert!(matches!(o, CallOutcome::Committed { .. }), "expected commit, got {o:?}");
}
fn read(engine: &Engine<MemoryStore>, field: &str) -> Option<Value> {
    let view = engine.view_at_head("readout").expect("view ok").expect("readout declared");
    view.rows().first().and_then(|row| row.field(field).cloned())
}
fn roundtrip(instance: &str, engine: &Engine<MemoryStore>) -> Engine<MemoryStore> {
    let artifact = engine.export().expect("export");
    Engine::restore(store(instance), &artifact, &mut generator()).expect("restore")
}

/// §8.2/§19.10: an optional singleton field never written reads `none` before and
/// after the round-trip. The optional-wrapper decode discipline must keep `none`
/// as `none`, not drop the member or fault it.
const NONE_APP: &str = r#"{
  "$liasse": 1, "$app": "t.none@1.0.0",
  "$model": {
    "note": { "$type": "text", "$optional": true },
    "n2": { "$type": "text", "$optional": true },
    "readout": { "$view": ". { note, n2 }" }
  },
  "$data": { "note": "seed" }
}"#;

#[test]
fn optional_singleton_none_roundtrips() {
    let engine = Engine::load(store("s-none"), NONE_APP, &mut generator()).expect("loads");
    assert_eq!(read(&engine, "note"), Some(text("seed")));
    assert_eq!(read(&engine, "n2"), None, "an unwritten optional singleton reads none");
    let restored = roundtrip("s-none", &engine);
    assert_eq!(read(&restored, "note"), Some(text("seed")), "seeded optional survives round-trip");
    assert_eq!(read(&restored, "n2"), None, "§19.10: an unwritten optional singleton stays none across restore");
}

/// §8.2/§19.10: a nested static struct holding a scalar `$ref`, and a `$set`-of-`$ref`
/// at both the root and inside a struct, round-trip their full ref values.
const REF_APP: &str = r#"{
  "$liasse": 1, "$app": "t.sref@1.0.0",
  "$model": {
    "accounts": { "$key": "id", "id": "text" },
    "config": { "owner": { "$ref": "/accounts" } },
    "admins": { "$set": { "$ref": "/accounts" } },
    "team": { "leads": { "$set": { "$ref": "/accounts" } } },
    "readout": { "$view": ". { owner: .config.owner, admins, leads: .team.leads }" }
  },
  "$data": {
    "accounts": { "a1": {}, "a2": {} },
    "config": { "owner": "a1" },
    "admins": ["a1"],
    "team": { "leads": ["a1","a2"] }
  }
}"#;

#[test]
fn singleton_ref_and_setref_roundtrip() {
    let engine = Engine::load(store("s-ref"), REF_APP, &mut generator()).expect("loads");
    let (owner, admins, leads) = (read(&engine, "owner"), read(&engine, "admins"), read(&engine, "leads"));
    assert!(owner.is_some() && admins.is_some() && leads.is_some(), "all ref members materialize");
    let restored = roundtrip("s-ref", &engine);
    assert_eq!(read(&restored, "owner"), owner, "nested-struct scalar ref round-trips");
    assert_eq!(read(&restored, "admins"), admins, "root set-of-ref round-trips");
    assert_eq!(read(&restored, "leads"), leads, "nested-struct set-of-ref round-trips");
}

/// §8.2/§19.10: `$normalize`/`$check` on a singleton survive the round-trip AND
/// stay enforced on the restored instance (an empty email rejects via `$check`).
const CHECK_APP: &str = r#"{
  "$liasse": 1, "$app": "t.chk@1.0.0",
  "$model": {
    "email": { "$type": "text", "$normalize": "string.lower(string.trim(.))", "$check": ["size(.) > 0", "required"] },
    "readout": { "$view": ". { email }" },
    "$mut": { "set_email": ".email = @v" }
  },
  "$data": { "email": "  Alice@EXAMPLE.com  " }
}"#;

#[test]
fn singleton_normalize_and_check_survive_roundtrip() {
    let engine = Engine::load(store("s-chk"), CHECK_APP, &mut generator()).expect("loads");
    assert_eq!(read(&engine, "email"), Some(text("alice@example.com")), "normalize applied at seed");
    let mut restored = roundtrip("s-chk", &engine);
    assert_eq!(read(&restored, "email"), Some(text("alice@example.com")), "normalized singleton round-trips");
    let mut g = generator();
    let bad = restored.call(&CallRequest::new("set_email").arg("v", text("   ")), &mut g).expect("call");
    assert!(
        matches!(bad, CallOutcome::Rejected(_)),
        "§8.2: the singleton `$check` stays enforced after restore (empty email rejects), got {bad:?}",
    );
}

/// §19.8: importing an earlier point as a rollback restores that point's singleton
/// value, not the current one.
const RB_APP: &str = r#"{
  "$liasse": 1, "$app": "t.rb@1.0.0",
  "$model": {
    "flag": "text",
    "readout": { "$view": ". { flag }" },
    "$mut": { "set_flag": ".flag = @v" }
  },
  "$data": { "flag": "A" }
}"#;

#[test]
fn rollback_restores_earlier_singleton_value() {
    let mut engine = Engine::load(store("s-rb"), RB_APP, &mut generator()).expect("loads");
    let early = engine.export().expect("export early");
    let mut g = generator();
    commit(engine.call(&CallRequest::new("set_flag").arg("v", text("B")), &mut g).expect("set"));
    assert_eq!(read(&engine, "flag"), Some(text("B")));
    assert_eq!(engine.classify(&early).expect("classify"), ImportRelation::Rollback);
    let report = engine.import(&early, &[ImportRelation::Rollback]).expect("import");
    assert!(report.applied);
    assert_eq!(read(&engine, "flag"), Some(text("A")), "§19.8: rollback restores the earlier singleton value");
}

/// §19.9: the singleton participates in the three-way merge. A change to the
/// singleton on one side and a disjoint collection insert on the other merge
/// cleanly and combine; when BOTH sides change the same singleton field it is a
/// conflict.
///
/// §19.9/§D.3 (SPEC-ISSUES #36): the singleton field conflict is reported at the
/// member's name-only application address — `ConflictCoordinate::RootSingleton {
/// member: Some("flag") }`, rendering `/flag` — never the internal reserved
/// `$root` name or its placeholder empty key (which is not a well-formed D.3 path).
const MG_APP: &str = r#"{
  "$liasse": 1, "$app": "t.mg@1.0.0",
  "$model": {
    "flag": "text",
    "notes": { "$key": "id", "id": "text" },
    "readout": { "$view": ". { flag }" },
    "$mut": { "set_flag": ".flag = @v", "add_note": ".notes + { id: @id }" }
  },
  "$data": { "flag": "base" }
}"#;

#[test]
fn singleton_merge_combines_and_conflicts_correctly() {
    let base = Engine::load(store("s-mg"), MG_APP, &mut generator()).expect("base").export().expect("base art");

    // incoming: disjoint change (adds a note, leaves flag=base).
    let mut incoming_engine = Engine::load(store("s-mg-i"), MG_APP, &mut generator()).expect("inc");
    let mut gi = generator();
    commit(incoming_engine.call(&CallRequest::new("add_note").arg("id", text("n1")), &mut gi).expect("add"));
    let incoming = incoming_engine.export().expect("inc art");

    // local: changes the singleton (flag -> local), no note.
    let mut local = Engine::load(store("s-mg"), MG_APP, &mut generator()).expect("local");
    let mut gl = generator();
    commit(local.call(&CallRequest::new("set_flag").arg("v", text("local")), &mut gl).expect("set"));

    let clean = local.merge(&base, &incoming).expect("merge");
    assert!(clean.is_clean(), "§19.9: a singleton change + a disjoint collection insert merge cleanly");
    // The combined result carries BOTH the local singleton value and the incoming note.
    let has_local_flag = clean
        .merged
        .values()
        .any(|f| f.get("flag") == Some(&text("local")));
    let has_note = clean.merged.values().any(|f| f.get("id") == Some(&text("n1")));
    assert!(has_local_flag, "merged result keeps the local singleton change");
    assert!(has_note, "merged result keeps the incoming collection insert");

    // CONFLICT: both sides change the same singleton field to different values.
    let mut incoming2 = Engine::load(store("s-mg-i2"), MG_APP, &mut generator()).expect("inc2");
    let mut gi2 = generator();
    commit(incoming2.call(&CallRequest::new("set_flag").arg("v", text("incoming")), &mut gi2).expect("set"));
    let incoming2_art = incoming2.export().expect("inc2 art");
    let conflicted = local.merge(&base, &incoming2_art).expect("merge2");
    assert!(
        !conflicted.is_clean(),
        "§19.9: both sides changing the same singleton field is a conflict, not a silent pick",
    );
    // §19.9/§D.3 (#36): the conflict coordinate is the member's name-only root
    // address, never the internal `$root`/empty-key storage row.
    assert_eq!(
        conflicted.conflicts.iter().map(|c| &c.coordinate).collect::<Vec<_>>(),
        vec![&ConflictCoordinate::RootSingleton { member: Some("flag".to_owned()) }],
        "§D.3: a singleton member conflict reports `/flag`, not the reserved `$root` name",
    );
}

/// §19.10 regression: a collections-only package (no singleton reserved row) still
/// round-trips, so Fix 3 introduced no spurious `$root` handling that breaks the
/// singleton-free case.
const NOSINGLE_APP: &str = r#"{
  "$liasse": 1, "$app": "t.ns@1.0.0",
  "$model": {
    "notes": { "$key": "id", "id": "text" },
    "all": { "$view": ".notes { id, $sort: [id] }" },
    "$mut": { "add": ".notes + { id: @id }" }
  }
}"#;

#[test]
fn collections_only_package_still_roundtrips() {
    let mut engine = Engine::load(store("s-ns"), NOSINGLE_APP, &mut generator()).expect("loads");
    let mut g = generator();
    commit(engine.call(&CallRequest::new("add").arg("id", text("n1")), &mut g).expect("add"));
    let artifact = engine.export().expect("export");
    let restored = Engine::restore(store("s-ns"), &artifact, &mut generator()).expect("restore");
    let view = restored.view_at_head("all").expect("view").expect("declared");
    let ids: Vec<String> = view
        .rows()
        .iter()
        .filter_map(|r| match r.field("id") {
            Some(Value::Text(t)) => Some(t.as_str().to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec!["n1"], "a collections-only package round-trips unchanged");
}
