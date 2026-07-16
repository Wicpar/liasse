//! Matcher parsing and matching on externally-authored examples. Each expected
//! outcome is deducible from FORMAT.md's matcher table, not from running the
//! matcher, so the assertions are not tautological.

use liasse_testkit::{Bindings, Matcher};
use serde_json::{json, Value};

fn matches(expected: Value, observed: Value) -> bool {
    let mut env = Bindings::new();
    Matcher::parse(&expected).check(&observed, &mut env).is_ok()
}

#[test]
fn any_matches_everything_and_literals_are_exact() {
    assert!(matches(json!("$any"), json!({"deep": [1, 2]})));
    assert!(matches(json!("hello"), json!("hello")));
    assert!(!matches(json!("hello"), json!("world")));
    assert!(matches(json!(7), json!(7)));
    assert!(!matches(json!(7), json!(8)));
}

#[test]
fn any_uuid_and_timestamp_check_shape() {
    assert!(matches(json!("$any:uuid"), json!("2f1c8b4a-1111-4222-8333-444455556666")));
    assert!(!matches(json!("$any:uuid"), json!("not-a-uuid")));
    assert!(matches(json!("$any:timestamp"), json!("1767225600000000")));
    assert!(matches(json!("$any:timestamp"), json!("2026-01-01T00:00:00Z")));
    assert!(!matches(json!("$any:timestamp"), json!("")));
}

#[test]
fn bind_then_ref_enforces_equality() {
    let mut env = Bindings::new();
    let first = Matcher::parse(&json!({"id": "$bind:t1", "title": "x"}));
    assert!(first.check(&json!({"id": "abc", "title": "x"}), &mut env).is_ok());

    // A later step sends the bound value back and must see the same value.
    assert!(Matcher::parse(&json!("$ref:t1")).check(&json!("abc"), &mut env).is_ok());
    assert!(Matcher::parse(&json!("$ref:t1")).check(&json!("different"), &mut env).is_err());
    // An unbound ref cannot match.
    assert!(Matcher::parse(&json!("$ref:missing")).check(&json!("abc"), &mut env).is_err());
}

#[test]
fn absent_requires_missing_member_and_exact_objects_reject_extras() {
    assert!(matches(json!({"a": 1, "b": "$absent"}), json!({"a": 1})));
    assert!(!matches(json!({"a": 1, "b": "$absent"}), json!({"a": 1, "b": 2})));
    // Exact object: an unlisted member fails.
    assert!(!matches(json!({"a": 1}), json!({"a": 1, "extra": 2})));
    // The open-object marker admits extras.
    assert!(matches(json!({"a": 1, "...": true}), json!({"a": 1, "extra": 2})));
    // A missing expected member fails.
    assert!(!matches(json!({"a": 1, "b": 2}), json!({"a": 1})));
}

#[test]
fn unordered_matches_as_a_set() {
    assert!(matches(json!({"$unordered": ["a", "b", "c"]}), json!(["c", "a", "b"])));
    assert!(!matches(json!({"$unordered": ["a", "b"]}), json!(["a", "c"])));
    // Cardinality is enforced.
    assert!(!matches(json!({"$unordered": ["a", "b"]}), json!(["a", "b", "b"])));
    // An ordered array, by contrast, is position-sensitive.
    assert!(!matches(json!(["a", "b"]), json!(["b", "a"])));
}

#[test]
fn unordered_binds_across_a_consistent_assignment() {
    // Two binders over a set must find an assignment that also satisfies a
    // later exact-value element, exercising the backtracking.
    let mut env = Bindings::new();
    let matcher = Matcher::parse(&json!({"$unordered": ["$bind:x", 2]}));
    assert!(matcher.check(&json!([2, 5]), &mut env).is_ok());
    assert_eq!(env.get("x"), Some(&json!(5)));
}

#[test]
fn resolve_substitutes_bound_refs_in_outgoing_args() {
    let mut env = Bindings::new();
    assert!(Matcher::parse(&json!("$bind:tok")).check(&json!("secret"), &mut env).is_ok());
    let args = json!({"credential": "$ref:tok", "keep": "$ref:unbound"});
    assert_eq!(env.resolve(&args), json!({"credential": "secret", "keep": "$ref:unbound"}));
}
