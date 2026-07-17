#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Red-team probe: `ref in view` membership (§6.3, §7.6).

mod common;

use common::{collection, eval, keyed_row, keyless_row, row_type, scalar, scell, vtext, view, FixedEnv, FixedScope};
use liasse_expr::{Cell, ExprType};
use liasse_value::{Ref, RefTarget, Type, Value};

#[test]
fn ref_needle_membership_in_view_compares_by_target_key() {
    // §7.6: "A ref value is a target key." §6.3 line 704: "Equality between a row
    // or ref and a key of the same declared target compares the current typed
    // key." Membership `in` is repeated equality, so a `ref<people>` needle tested
    // against the `.people` view compares the ref's target key against the view's
    // row identity keys. `owner` refs person "b", which exists in `.people`, so
    // membership MUST be true. Externally derived from the identity rule, not from
    // the implementation.
    let people_row = row_type(vec![("id", scalar(Type::Text))], Some(scalar(Type::Text)));
    let ref_ty = Type::Ref(RefTarget::Scalar(Box::new(Type::Text)));
    let root_ty = row_type(
        vec![("people", view(people_row)), ("owner", scalar(ref_ty))],
        None,
    );
    let scope = FixedScope::new(ExprType::Row(root_ty));

    let person = |k: &str| keyed_row(k, vtext(k), vec![("id", scell(vtext(k)))]);
    let root = keyless_row(
        0,
        vec![
            ("people", collection(vec![person("a"), person("b"), person("c")])),
            ("owner", scell(Value::Ref(Ref::scalar(vtext("b"))))),
        ],
    );
    let dot = Cell::Row(Box::new(root.clone()));
    let env = FixedEnv::new(root);

    // Control: the same identity compared with `==` DOES unwrap the ref.
    let eq = eval(&scope, &env, &dot, ".owner == .people['b'].id");
    assert_eq!(eq.as_scalar(), Some(&Value::Bool(true)), "ref == key must hold");

    // The bug: `ref in view` never matches because the ref is not unwrapped.
    let present = eval(&scope, &env, &dot, ".owner in .people");
    assert_eq!(
        present.as_scalar(),
        Some(&Value::Bool(true)),
        "a ref whose target exists in the view must be a member"
    );
}
