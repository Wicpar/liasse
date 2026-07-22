//! RUNTIME CONTROL for the WAVE-4 harness receiver-reconstruction bug (W4-F7).
//!
//! The testkit harness reconstructed a depth-≥2 receiver key by dropping every
//! ancestor selector, so `.companies[@company].accounts[@account].add_note`
//! addressed the account with a 1-component key and the runtime rejected it
//! `Malformed`. That was a HARNESS bug — this control proves the RUNTIME itself
//! admits the full multi-component receiver key.
//!
//! §8.2: a nested collection row's canonical key is its full ancestor path;
//! §10.1: a mutation declared on `accounts` (nested under `companies`) is a
//! legal row-mutation whose receiver is that account row. So an `Engine::call`
//! for `add_note` carrying the receiver key `[co, a1]` (each ancestor key
//! component appended in `$key` order) MUST admit and mutate the addressed
//! account — with NO meter in play, admission is purely receiver resolution.
//!
//! Externally deducible: the seed holds `companies[co].accounts[a1]`; a spend of
//! `add_note("hi")` against that account appends one note and returns its `id`.
//! The one-component control (`[co]`) is a `Malformed` rejection precisely
//! because `accounts` sits one level below `companies` — proving the runtime
//! enforces the full-path key the harness must reconstruct.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Value};

use support::{generator, store};

fn text(value: &str) -> Value {
    Value::Text(liasse_value::Text::new(value.to_owned()))
}

// companies[id].accounts[id].notes[id]; `add_note` is a receiver-bound mutation
// on the NESTED `accounts` collection (so its receiver key is `[company,
// account]`). No meters.
const APP: &str = r#"{
  "$liasse": 1,
  "$app": "t.rt.nestrecv.control@1.0.0",
  "$model": {
    "companies": {
      "$key": "id",
      "id": "text",
      "accounts": {
        "$key": "id",
        "id": "text",
        "notes": {
          "$key": "id",
          "id": "uuid = uuid()",
          "text": "text"
        },
        "$mut": {
          "add_note": [ "note = .notes + { text: @text }", "return note { id }" ]
        }
      }
    }
  },
  "$data": { "companies": { "co": { "accounts": { "a1": {} } } } }
}"#;

#[test]
fn full_nested_receiver_key_admits() {
    let mut generator = generator();
    let mut engine = match liasse_runtime::Engine::load(store("nested-recv"), APP, &mut generator) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error:?}"),
    };

    // The FULL 2-component receiver key `[co, a1]`: each ancestor key component
    // appended in `$key` order addresses the nested account row (§8.2/§10.1).
    let admitted = engine
        .call(
            &CallRequest::new("add_note")
                .receiver(text("co"))
                .receiver(text("a1"))
                .arg("text", text("hi")),
            &mut generator,
        )
        .expect("engine ok");
    assert!(
        matches!(admitted, CallOutcome::Committed { .. }),
        "the runtime admits the full multi-component receiver key `[co, a1]` \
         (§8.2/§10.1); the harness bug was dropping ancestor selectors, not the \
         runtime: got {admitted:?}",
    );

    // A SHORT 1-component key `[a1]` for the 2-level path is `Malformed` — this is
    // exactly the state the buggy harness reconstruction put the runtime in, so it
    // confirms the runtime is the enforcer and the reconstruction is the fault.
    let short = engine
        .call(
            &CallRequest::new("add_note").receiver(text("a1")).arg("text", text("late")),
            &mut generator,
        )
        .expect("engine ok");
    assert!(
        matches!(short, CallOutcome::Rejected(_)),
        "a 1-component receiver key for a 2-level path is malformed (§8.2): got {short:?}",
    );
}
