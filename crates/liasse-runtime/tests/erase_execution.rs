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
