//! Move-operator evaluation (SPEC §8.5): `dest <- source` and `source -> dest`
//! transfer the source value into the destination and unset the source binding;
//! `=` copies and leaves the source live. Both spellings are one move and produce
//! the same committed result. Routed through `Result` so the crate's
//! deny-by-default lints (no `unwrap`/`expect`/`panic`/indexing) hold here.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, load};

type Check = Result<(), String>;

const MOVE: &str = r#"{
  "$liasse": 1,
  "$app": "example.move@1.0.0",
  "$model": {
    "tasks": { "$key": "id", "id": "uuid = uuid()", "title": "text" },
    "all": { "$view": ".tasks { id, title }" },
    "$mut": {
      "move_from": ["t = .tasks + { title: @title }", "held <- t", "return held { title }"],
      "move_to":   ["t = .tasks + { title: @title }", "t -> held", "return held { title }"],
      "copy_keeps_source": ["t = .tasks + { title: @title }", "held = t", "return t { title }"]
    }
  }
}"#;

/// Call `mutation` with `title`, returning its outcome.
fn call(
    engine: &mut Engine<MemoryStore>,
    mutation: &str,
    title: &str,
) -> Result<CallOutcome, String> {
    let mut generator = generator();
    engine
        .call(
            &CallRequest::new(mutation).arg("title", Value::Text(Text::new(title))),
            &mut generator,
        )
        .map_err(|error| format!("call `{mutation}` failed: {error:?}"))
}

/// The committed response's wire form, or an error if the call did not commit a
/// response.
fn committed_response(outcome: CallOutcome) -> Result<serde_json::Value, String> {
    match outcome {
        CallOutcome::Committed { response, .. } => Ok(response
            .ok_or("the mutation returned no response")?
            .to_wire()),
        other => Err(format!("expected a committed move, got {other:?}")),
    }
}

#[test]
fn move_from_transfers_the_value_to_the_destination() -> Check {
    // §8.5: `held <- t` transfers `t`'s value into `held`; the returned projection
    // of `held` therefore carries the title that was moved into it.
    let mut engine = load("move-from", MOVE);
    let response = committed_response(call(&mut engine, "move_from", "Alpha")?)?;
    assert_eq!(response, serde_json::json!({ "title": "Alpha" }));
    Ok(())
}

#[test]
fn move_to_spelling_transfers_the_same_way() -> Check {
    // §8.5: `t -> held` is the mirror spelling of `held <- t` and lands the value
    // in `held` identically.
    let mut engine = load("move-to", MOVE);
    let response = committed_response(call(&mut engine, "move_to", "Beta")?)?;
    assert_eq!(response, serde_json::json!({ "title": "Beta" }));
    Ok(())
}

#[test]
fn both_move_spellings_commit_the_same_result() -> Check {
    // The two spellings are one move: over the same input they produce identical
    // responses.
    let mut a = load("move-eq-a", MOVE);
    let mut b = load("move-eq-b", MOVE);
    let from = committed_response(call(&mut a, "move_from", "Gamma")?)?;
    let to = committed_response(call(&mut b, "move_to", "Gamma")?)?;
    assert_eq!(from, to, "`<-` and `->` must commit the same result");
    Ok(())
}

#[test]
fn a_move_commits_the_staged_insert() -> Check {
    // The local-handle move does not disturb the row the program staged: the
    // committed collection still holds the inserted task.
    let mut engine = load("move-commit", MOVE);
    let _ = committed_response(call(&mut engine, "move_from", "Delta")?)?;
    let view = engine
        .view_at_head("all")
        .map_err(|error| format!("view: {error:?}"))?
        .ok_or("the `all` view is not declared")?;
    assert_eq!(view.len(), 1, "the inserted row is committed");
    let row = view.rows().first().ok_or("no committed row")?;
    assert_eq!(
        row.field("title").map(Value::to_wire),
        Some(serde_json::json!("Delta"))
    );
    Ok(())
}

#[test]
fn equals_copies_and_leaves_the_source_readable() -> Check {
    // §8.5: `=` COPIES, so after `held = t` the source `t` is still live — the
    // program returns a projection of `t` itself. A move (`held <- t`) would have
    // consumed `t`, and reading it would be a load-time use-after-move; the copy
    // does not.
    let mut engine = load("copy-keeps", MOVE);
    let response = committed_response(call(&mut engine, "copy_keeps_source", "Epsilon")?)?;
    assert_eq!(response, serde_json::json!({ "title": "Epsilon" }));
    Ok(())
}
