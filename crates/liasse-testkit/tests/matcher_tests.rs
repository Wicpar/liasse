//! Matcher parsing and matching on externally-authored examples. Each expected
//! outcome is deducible from FORMAT.md's matcher table, not from running the
//! matcher, so the assertions are not tautological.

use liasse_testkit::{Bindings, MatchError, Matcher};
use serde_json::{json, Value};

fn matches(expected: Value, observed: Value) -> bool {
    let mut env = Bindings::new();
    Matcher::parse(&expected).check(&observed, &mut env).is_ok()
}

/// The divergence report for a known mismatch, or an error if it unexpectedly
/// matched. Paths follow FORMAT.md's `$.member[index]` convention, so the
/// expected path is externally deducible, not read back from the matcher.
fn diverge(expected: Value, observed: Value) -> Result<MatchError, String> {
    let mut env = Bindings::new();
    match Matcher::parse(&expected).check(&observed, &mut env) {
        Ok(()) => Err(format!("expected a mismatch for {expected} vs {observed}")),
        Err(err) => Ok(err),
    }
}

#[test]
fn divergence_reports_the_exact_failing_path() -> Result<(), String> {
    // A nested literal mismatch is located at the offending array element.
    let err = diverge(json!({"a": {"b": [1, 2]}}), json!({"a": {"b": [1, 3]}}))?;
    assert_eq!(err.path, "$.a.b[1]", "got {err}");
    assert!(err.reason.contains("literal value mismatch"), "got {err}");

    // A missing expected member is located at that member.
    let err = diverge(json!({"a": 1, "b": 2}), json!({"a": 1}))?;
    assert_eq!(err.path, "$.b", "got {err}");
    assert!(err.reason.contains("missing"), "got {err}");

    // An unexpected extra member is reported at the enclosing object.
    let err = diverge(json!({"a": 1}), json!({"a": 1, "extra": 2}))?;
    assert_eq!(err.path, "$", "got {err}");
    assert!(err.reason.contains("extra members"), "got {err}");

    // An array-length divergence is reported at the array path.
    let err = diverge(json!({"xs": [1, 2]}), json!({"xs": [1]}))?;
    assert_eq!(err.path, "$.xs", "got {err}");
    assert!(err.reason.contains("expected 2 elements, found 1"), "got {err}");

    // A typed-shape mismatch (uuid) is located at the member.
    let err = diverge(json!({"id": "$any:uuid"}), json!({"id": "nope"}))?;
    assert_eq!(err.path, "$.id", "got {err}");
    assert!(err.reason.contains("uuid"), "got {err}");
    Ok(())
}

#[test]
fn ref_divergence_names_the_binding() -> Result<(), String> {
    let mut env = Bindings::new();
    if Matcher::parse(&json!("$bind:t")).check(&json!("A"), &mut env).is_err() {
        return Err("bind should succeed".into());
    }
    let Err(err) = Matcher::parse(&json!({"id": "$ref:t"})).check(&json!({"id": "B"}), &mut env) else {
        return Err("a differing ref must diverge".into());
    };
    assert_eq!(err.path, "$.id", "got {err}");
    assert!(err.reason.contains('t'), "reason should name the binding, got {err}");
    Ok(())
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

#[test]
fn int_number_and_canonical_wire_string_denote_the_same_value() {
    // Annex A.1 renders `int` as a canonical base-10 JSON string; the Annex-B
    // ordering corpus authors int expectations as bare numbers (its NOTES.md
    // "Scalar wire forms"). Both spellings of the same integer must match, in
    // either authoring direction, including a negative value.
    assert!(matches(json!(-10), json!("-10")));
    assert!(matches(json!("-10"), json!(-10)));
    assert!(matches(json!(0), json!("0")));
    assert!(matches(json!(100), json!("100")));
    // Nested inside a row object, as the view assertions deliver it.
    assert!(matches(json!([{ "id": "d", "n": -10 }]), json!([{ "id": "d", "n": "-10" }])));
}

#[test]
fn int_coercion_rejects_non_canonical_and_unequal_forms() {
    // A non-canonical spelling (leading zero, `+`, negative zero) is not the
    // canonical int wire form, so it does not coerce.
    assert!(!matches(json!(10), json!("010")));
    assert!(!matches(json!(0), json!("-0")));
    // Different integer values never match across forms.
    assert!(!matches(json!(2), json!("10")));
    // A text field that happens to read as digits is not coerced to a number
    // unless the number equals it exactly (distinct values stay distinct).
    assert!(!matches(json!("abc"), json!(1)));
    // A non-integer number is never coerced to a string.
    assert!(!matches(json!(1.5), json!("1.5")));
}
