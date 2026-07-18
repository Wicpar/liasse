//! Red-team regression: the direct `collection - { object }` delete (§8.5) must
//! validate its composite-key **object operand** at load exactly like the
//! `[{..}]` selector, `==`, and `in` do. Today it does not: a wrong-typed,
//! wrong-arity, or extra-field composite operand type-checks and loads, so the
//! author's typo is caught nowhere — at runtime it silently no-ops (missing /
//! wrong-typed component) or silently deletes anyway (extra non-key field).
//!
//! Why this is a defect (all normative):
//!   * §6 intro (line 634): an expression's "static type and effect class are
//!     checked when the package is loaded", and the sentence names *mutations*
//!     and *selectors* explicitly. A statically ill-typed key operand is a
//!     load-time rejection.
//!   * §6.3 (line 698): "A composite-key lookup uses one object operand naming
//!     each key component." An operand missing a component, carrying an extra
//!     field, or supplying a component of the wrong type is not such a lookup.
//!   * §8.5: `collection - keys` deletes rows by key; "keys" is the same key
//!     source the selector uses, so the same key must be a key of the target.
//!   * A.9: a named object selector is authoring syntax for the target's
//!     `$key`-order typed tuple, carrying each component's declared type. An
//!     `int` where a `text` component is declared, a 1-tuple where a 2-tuple is
//!     declared, or a stray non-component field is not a key of the target.
//!
//! The cluster fix 4dea8d3 states `Checker::coerce_composite_key` is "the ONE
//! validate-and-normalize point ... applied in EVERY position an object key
//! operand can appear", and lists "`collection - keys` delete" as one of them.
//! But for the direct-delete form it wired only §8.3 parameter inference
//! (`infer_composite_key`) and the runtime carrier normalize
//! (`materialize::normalize_key_operand`) — never the load-time validation:
//! `mutation::type_value` returns `None` for any `uses_mutation_operator` form,
//! so the delete operand never reaches the checker where `coerce_composite_key`
//! lives. Selector / `==` / `in` reject the identical operands at load; the
//! direct-delete form is the one position the fix left unguarded.
//!
//! Expectations are derived from the spec chain above and from the peer
//! red-teams `redteam_composite_in_typing` (the `in` analog) and
//! `redteam_composite_ref_compare_typing` (the `==` analog), not from the
//! implementation's current answer.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;

/// A `regions` collection with composite key `[region, code]` (both `text`) and
/// a mutation whose whole body is the direct-delete statement under test.
fn model_with_delete(delete_body: &str) -> String {
    format!(
        r#"{{ "$liasse": 1, "$app": "t.cdel@1.0.0", "$model": {{
            "regions": {{ "$key": ["region","code"], "region":"text", "code":"text" }},
            "$mut": {{ "probe": "{delete_body}" }}
        }} }}"#
    )
}

/// THE BUG — mismatched component type (§6.3, A.9, §6 intro). `code: 5` is `int`
/// where the target key component `code` is `text`. The `in` and `==` analogs
/// reject this at load; the direct-delete form MUST reject it too. At runtime the
/// operand instead builds `Value::Composite([Text("eu"), Int(5)])`, matches no
/// row, and the delete silently commits `Unchanged`.
#[test]
fn direct_delete_mismatched_component_type_must_reject() {
    let built = build(&model_with_delete(".regions - { region: 'eu', code: 5 }"));
    assert!(
        built.result.is_err(),
        "§6.3/A.9/§6-intro: `.regions - {{ region:'eu', code: 5 }}` supplies `code: 5` (int) \
         where the composite key component `code` is `text`; a static type mismatch checked at \
         load, exactly as the `in`/`==` analogs reject it. The model accepted it (the delete \
         then silently no-ops at runtime)."
    );
}

/// THE BUG — wrong arity (§6.3, A.9). A 1-component object against a 2-tuple
/// composite key is not a key of the target. The `in` analog rejects the
/// identical shape; the direct-delete form MUST too. At runtime the missing
/// component is filled with `Value::None`, so the delete matches no row and
/// silently no-ops.
#[test]
fn direct_delete_wrong_arity_must_reject() {
    let built = build(&model_with_delete(".regions - { region: 'eu' }"));
    assert!(
        built.result.is_err(),
        "§6.3/A.9: `.regions - {{ region:'eu' }}` names only 1 of the 2 `$key` components; a \
         1-tuple is not a key of a 2-tuple composite target and MUST reject at load, as the \
         `[{{..}}]`/`in` forms do. The model accepted it (the delete then silently no-ops)."
    );
}

/// THE BUG — extra non-key field (§6.3, A.9). An object carrying a field that is
/// not a `$key` component is not a key of the target. The `[{..}]`/`==`/`in`
/// forms reject the extra field at load; the direct-delete form MUST too. This
/// variant is the more dangerous one: at runtime `normalize_key_operand` ignores
/// the stray field, so the delete silently *succeeds* on a malformed operand.
#[test]
fn direct_delete_extra_field_must_reject() {
    let built = build(&model_with_delete(".regions - { region: 'eu', code: 'x', bogus: 'z' }"));
    assert!(
        built.result.is_err(),
        "§6.3/A.9: `.regions - {{ region, code, bogus }}` carries `bogus`, not a `$key` \
         component; an object operand must name exactly the key components (A.9 arity), so this \
         MUST reject at load as the selector/`==`/`in` forms do. The model accepted it (the \
         delete then silently deletes anyway, ignoring `bogus`)."
    );
}

/// Control — the `in` analog of the mismatched-type case DOES reject at load
/// (per `redteam_composite_in_typing`). This anchors the rejections above to the
/// shared coercion rule, isolating the defect to the direct-delete *position*,
/// not to composite object operands in general.
#[test]
fn control_in_analog_rejects_mismatched_type() {
    let def = r#"{ "$liasse": 1, "$app": "t.cdel@1.0.0", "$model": {
        "regions": { "$key": ["region","code"], "region":"text", "code":"text" },
        "$mut": { "probe": "assert({ region: 'eu', code: 5 } in .regions, 'must exist')" }
    } }"#;
    assert!(
        build(def).result.is_err(),
        "control: the `in` position already rejects `code: 5` (int vs text) at load; the \
         direct-delete position must match it."
    );
}

/// Control — a correctly-typed, exact-arity composite delete operand loads. This
/// proves the three rejections above target the type/arity/extra-field check and
/// not the direct-delete form as such (it is a supported form; see the runtime
/// peer `redteam_composite_direct_delete`).
#[test]
fn control_correct_composite_delete_operand_loads() {
    build(&model_with_delete(".regions - { region: 'eu', code: 'x' }")).expect_ok();
}
