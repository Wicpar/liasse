#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! A.6 (SPEC.md:4437): decimal division must retain at least sixteen
//! *significant* fractional digits, following PostgreSQL numeric. The impl
//! selects sixteen *fractional* digits instead, losing precision whenever the
//! quotient has leading fractional zeros. This is a value/rounding divergence,
//! distinct from the unpinned trailing-zero spelling (SPEC-ISSUES item 1).

mod common;

use common::{as_scalar, eval, vint, FixedEnv, FixedScope};
use liasse_expr::{Cell, ExprType};
use liasse_value::bigdecimal::BigDecimal;
use liasse_value::{Type, Value};
use std::str::FromStr;

#[test]
fn decimal_division_keeps_sixteen_significant_fractional_digits() {
    let scope = FixedScope::new(ExprType::scalar(Type::Int));
    let env = FixedEnv::new(common::keyless_row(0, vec![]));
    let dot = Cell::Scalar(vint(0));
    // 1/700000 = 0.00000142857142857142857...; sixteen SIGNIFICANT fractional
    // digits requires scale >= 21. A conforming result reproduces 1/700000 to
    // ~16 sig figs, so R*700000 == 1 to under 1e-13 (a 16-sig result deviates by
    // ~3.5e-16). The impl yields 0.0000014285714286 (~11 sig figs).
    let r = match as_scalar(&eval(&scope, &env, &dot, "1.0 / 700000.0")) {
        Value::Decimal(d) => d.as_big_decimal().clone(),
        other => panic!("expected decimal, got {other:?}"),
    };
    let recon = &r * BigDecimal::from(700000);
    let err = (&recon - BigDecimal::from(1)).abs();
    let tol = BigDecimal::from_str("0.0000000000001").unwrap(); // 1e-13
    assert!(
        err < tol,
        "1.0/700000.0 = {r}; R*700000 = {recon} deviates from 1 by {err} \
         (fewer than 16 significant fractional digits)"
    );
}
