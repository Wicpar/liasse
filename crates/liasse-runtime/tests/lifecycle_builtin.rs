#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! The host-privileged `module.*` lifecycle builtin (§13.10) — privilege refusal.
//!
//! A `module.install`/`module.update`/`module.remove` call is recognised
//! structurally by the interpreter (like `#handle.mut`). It is served ONLY when the
//! host/root-scope lifecycle handle is lent. A plain single-engine `Engine::call`
//! lends no such handle, so the call is refused LOUDLY and the whole transition
//! rejects — the parent's own state change never commits. This is the privilege
//! mechanism (refuse-when-absent) a non-host/non-root caller hits.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, RejectionReason, Value};
use liasse_value::Text;
use support::generator;

/// A root package whose `try_install` mutation makes its OWN change (`.log`) and
/// then invokes the host-privileged `module.install` builtin.
const PKG: &str = r#"{
  "$liasse": 1
  "$app": "t.lifecycle.priv@1.0.0"
  "$model": {
    "log": { "$key": "id", "id": "text" }
    "log_view": { "$view": ".log { id }" }
    "$mut": {
      "try_install": [
        "e = .log + { id: @id }"
        "module.install({ name: 'child' })"
        "return e { id }"
      ]
    }
  }
}"#;

#[test]
fn a_single_engine_caller_is_refused_the_lifecycle_builtin() {
    let mut engine = support::load("t.lifecycle.priv", PKG);
    let request = CallRequest::new("try_install").arg("id", Value::Text(Text::new("a")));

    let outcome = engine.call(&request, &mut generator()).expect("no engine fault");

    match outcome {
        CallOutcome::Rejected(rejection) => {
            assert_eq!(
                rejection.reason(),
                RejectionReason::Malformed,
                "an unprivileged lifecycle call is a Malformed refusal"
            );
            assert!(
                rejection.message().contains("host-privileged")
                    && rejection.message().contains("module.install"),
                "the refusal names the host-privileged builtin: {}",
                rejection.message()
            );
        }
        other => panic!("expected a loud refusal, got {other:?}"),
    }

    // The whole transition rejected: the parent's own `.log` insert never committed.
    let view = engine.view_at_head("log_view").expect("view").expect("log_view exists");
    assert!(view.rows().is_empty(), "no row commits when the lifecycle call is refused");
}
