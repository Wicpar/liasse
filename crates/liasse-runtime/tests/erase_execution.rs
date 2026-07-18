#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §21.2 erase execution through `Engine::call`: a `return erase(row)` mutation
//! plans the same live removal ordinary deletion would (step 1), scrubs the
//! retained payload, commits, and returns the durable extract (step 6). The
//! erased row is then unobservable in live views and absent from a fresh export.

mod support;

use liasse_ident::InstanceId;
use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, load};

/// A minimal package whose `erase_user` mutation is exactly `return erase(row)`
/// (the §21.2 corpus shape), plus an `add` insert and an `all` view to observe
/// live state.
const USERS: &str = r#"{
  "$liasse": 1
  "$app": "t.erase@1.0.0"
  "$model": {
    "users": { "$key": "id", "id": "text", "secret": "text" }
    "all": { "$view": ".users { id, secret, $sort: [id] }" }
    "$mut": {
      "add": ".users + { id: @id, secret: @secret }"
      "erase_user": "return erase(.users[@id])"
    }
  }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn add_user(engine: &mut Engine<MemoryStore>, id: &str, secret: &str) {
    let mut generator = generator();
    let request = CallRequest::new("add").arg("id", text(id)).arg("secret", text(secret));
    engine.call(&request, &mut generator).expect("add commits");
}

fn user_ids(engine: &Engine<MemoryStore>) -> Vec<String> {
    engine
        .view_at_head("all")
        .expect("view")
        .expect("declared")
        .rows()
        .iter()
        .map(|row| format!("{:?}", row.field("id").expect("id")))
        .collect()
}

#[test]
fn erase_commits_removes_the_row_and_returns_an_extract() {
    let mut engine = load("erase", USERS);
    add_user(&mut engine, "u1", "hunter2");
    add_user(&mut engine, "u2", "swordfish");
    assert_eq!(user_ids(&engine).len(), 2, "two users before the erase");

    let mut generator = generator();
    let request = CallRequest::new("erase_user").arg("id", text("u1"));
    let outcome = engine.call(&request, &mut generator).expect("erase call");

    // §21.2 step 6: the erase committed and returned the durable extract.
    let CallOutcome::Committed { response, .. } = outcome else {
        panic!("erase must commit a state change, got {outcome:?}");
    };
    let response = response.expect("erase returns the extract as its response");
    // The extract crosses the response boundary as its content hash (§21.3) — a
    // non-empty text — and never the scrubbed payload bytes.
    let wire = response.to_wire();
    let hash = wire.as_str().expect("the extract response is a content-hash text");
    assert!(!hash.is_empty(), "the extract carries a content hash");
    assert!(!hash.contains("hunter2"), "the scrubbed secret never re-leaks through the response");

    // §21.2 step 1: the erased row is gone from live state; the sibling survives.
    assert_eq!(user_ids(&engine), vec![format!("{:?}", text("u2"))], "only the non-erased user remains");
}

#[test]
fn erased_row_is_absent_from_a_fresh_export() {
    let mut engine = load("erase-export", USERS);
    add_user(&mut engine, "u1", "hunter2");

    let mut gen_erase = generator();
    let request = CallRequest::new("erase_user").arg("id", text("u1"));
    engine.call(&request, &mut gen_erase).expect("erase call").committed_at().expect("committed");

    // §21.2: because the removal flows through ordinary admission, the erased row
    // is absent from a fresh export — a restore into a clean runtime has no user,
    // and the scrubbed secret does not travel in the artifact bytes.
    let artifact = engine.export().expect("export");
    assert!(
        !artifact.windows(b"hunter2".len()).any(|w| w == b"hunter2"),
        "the erased payload is absent from the exported artifact",
    );

    let mut gen_restore = generator();
    let store = MemoryStore::new(InstanceId::new("erase-export"));
    let restored = Engine::restore(store, &artifact, &mut gen_restore).expect("restore");
    assert!(user_ids(&restored).is_empty(), "the erased row does not reappear after export/restore");
}

/// A package where erasing a user cascades (§21.1 `$on_delete: cascade`) to its
/// profile, which itself holds sensitive bytes — the §24 delete-closure erasure
/// shape.
const CASCADE: &str = r#"{
  "$liasse": 1
  "$app": "t.erasecascade@1.0.0"
  "$model": {
    "users": { "$key": "id", "id": "text" }
    "profiles": {
      "$key": "id"
      "id": "text"
      "bio": "text"
      "user": { "$ref": "/users", "$on_delete": "cascade" }
    }
    "users_all": { "$view": ".users { id, $sort: [id] }" }
    "profiles_all": { "$view": ".profiles { id, bio, $sort: [id] }" }
    "$mut": {
      "erase_user": "return erase(.users[@id])"
    }
  }
  "$data": {
    "users": { "u1": {} }
    "profiles": { "pr1": { "bio": "sensitive", "user": "u1" } }
  }
}"#;

fn row_count(engine: &Engine<MemoryStore>, view: &str) -> usize {
    engine.view_at_head(view).expect("view").expect("declared").rows().len()
}

/// §21.2/§21.1 (§24): erasing a user removes the SAME delete-closure an ordinary
/// deletion would — the user AND the cascade-reached profile — and commits one
/// durable extract. The scrubbed profile's sensitive bytes never re-leak through
/// the response, and a fresh export contains neither closure row.
#[test]
fn erase_removes_the_whole_cascade_closure_and_scrubs_it() {
    let mut engine = load("erase-cascade", CASCADE);
    assert_eq!(row_count(&engine, "users_all"), 1, "one user before the erase");
    assert_eq!(row_count(&engine, "profiles_all"), 1, "one profile before the erase");

    let mut gen_erase = generator();
    let outcome = engine
        .call(&CallRequest::new("erase_user").arg("id", text("u1")), &mut gen_erase)
        .expect("erase call");
    let CallOutcome::Committed { response, .. } = outcome else {
        panic!("erase must commit, got {outcome:?}");
    };
    // §21.2 step 6: the reintegration bundle is delivered as its content-hash text;
    // the scrubbed profile bytes never cross the response boundary.
    let response = response.expect("erase returns the extract");
    let hash = response.to_wire();
    let hash = hash.as_str().expect("the extract response is a content-hash text");
    assert!(!hash.is_empty(), "the bundle carries a content hash");
    assert!(!hash.contains("sensitive"), "the scrubbed profile bytes never re-leak");

    // §21.1 delete-closure: BOTH the direct target and the cascade profile left
    // live state — erasure's live scope is identical to deletion's, and the erase
    // captured the whole closure (not just the direct target) for the bundle.
    assert_eq!(row_count(&engine, "users_all"), 0, "the erased user is gone");
    assert_eq!(row_count(&engine, "profiles_all"), 0, "the cascade profile is gone");
    // (The durable byte-level scrub of the cascade row's RETAINED HISTORY, and its
    // observation, is the documented CORE history-durability seam — the export here
    // carries live state plus the definition text, so it is not the right lens for
    // the history stub; the closure's export coverage is pinned by the deletion-unit
    // test `erasure_export_covers_the_cascade_closure`.)
}
