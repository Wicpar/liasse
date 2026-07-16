#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! §7.5 avg divides "under the package semantics" (A.6): the quotient must keep
//! at least sixteen *significant* fractional digits. average() in
//! src/eval/aggregate.rs uses a 16-*fractional*-digit scale, losing precision on
//! means with leading fractional zeros.

mod common;

use common::{as_scalar, eval, vint, FixedEnv, FixedScope};
use liasse_expr::{Cell, ExprType};
use liasse_value::bigdecimal::BigDecimal;
use liasse_value::{Text, Type, Value};
use std::str::FromStr;

#[test]
fn avg_keeps_sixteen_significant_fractional_digits() {
    const N: u64 = 70000;
    let ty = common::row_type(
        vec![
            ("id", common::scalar(Type::Text)),
            ("amount", common::scalar(Type::Int)),
        ],
        Some(common::scalar(Type::Text)),
    );
    // one `1`, the rest `0`: sum = 1, count = 70000, avg = 1/70000.
    let mut rows = Vec::new();
    for n in 0..N {
        let amt = if n == 0 { 1 } else { 0 };
        rows.push(common::row(
            n + 1,
            Value::Text(Text::new(format!("k{n}"))),
            vec![
                ("id", Cell::Scalar(Value::Text(Text::new(format!("k{n}"))))),
                ("amount", Cell::Scalar(vint(amt))),
            ],
        ));
    }
    let root_ty = common::row_type(vec![("items", common::view(ty))], None);
    let scope = FixedScope::new(ExprType::Row(root_ty));
    let root = common::keyless_row(0, vec![("items", common::collection(rows))]);
    let dot = Cell::Row(Box::new(root.clone()));
    let env = FixedEnv::new(root);
    let r = match as_scalar(&eval(&scope, &env, &dot, "avg(.items.amount)")) {
        Value::Decimal(d) => d.as_big_decimal().clone(),
        other => panic!("expected decimal, got {other:?}"),
    };
    // 1/70000 needs scale >= 21 for 16 significant fractional digits; a
    // conforming avg*70000 == 1 to ~3.5e-16.
    let recon = &r * BigDecimal::from(N);
    let err = (&recon - BigDecimal::from(1)).abs();
    let tol = BigDecimal::from_str("0.0000000000001").unwrap(); // 1e-13
    assert!(
        err < tol,
        "avg = {r}; avg*{N} = {recon} deviates from 1 by {err} \
         (fewer than 16 significant fractional digits)"
    );
}
