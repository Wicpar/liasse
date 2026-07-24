//! The blob boundary and the lifecycle operators (§13.16).
//!
//! `unpack(blob)` reads a `.liasse` blob into a move-only `module` value, with
//! materialization DEFERRED — no decode or mount happens at `unpack`; the value
//! carries the source blob as a `Pending` handle.
//!
//! `pack`, `update_module` and `rollback_module` are typed here and REFUSED by the
//! pure evaluator: each carries an instance through its §13.10 lifecycle against
//! engines this crate cannot reach, so a pure position must never obtain a result
//! from one.
//!
//! Written without `unwrap`/`expect`/`panic!` so the workspace deny-lints hold:
//! fallible construction threads through `Result` + `?`.

mod common;

use common::{
    check, check_rejects, eval, keyless_row, row_type, scalar, scell, try_eval, FixedEnv,
    FixedScope,
};
use liasse_diag::SourceMap;
use liasse_expr::{Cell, EvalError, ExprType};
use liasse_value::{BlobDescriptor, MediaType, ModuleHandle, ModuleType, Sha512, Type, Value};

fn descriptor() -> Result<BlobDescriptor, String> {
    let sha = Sha512::parse(&"b".repeat(128)).map_err(|e| e.to_string())?;
    Ok(BlobDescriptor::new(
        sha,
        128,
        MediaType::new("application/vnd.liasse+zip"),
        Some("pkg.liasse".to_owned()),
    ))
}

/// A scope/env whose root exposes one `pkg` blob field.
fn with_blob(descriptor: BlobDescriptor) -> (FixedScope, FixedEnv, Cell) {
    let root_ty = row_type(vec![("pkg", scalar(Type::Blob))], None);
    let scope = FixedScope::new(ExprType::Row(root_ty));
    let root = keyless_row(0, vec![("pkg", scell(Value::Blob(Box::new(descriptor))))]);
    let dot = Cell::Row(Box::new(root.clone()));
    (scope, FixedEnv::new(root), dot)
}

#[test]
fn unpack_types_as_an_unrefined_module() -> Result<(), String> {
    let (scope, _env, _dot) = with_blob(descriptor()?);
    assert_eq!(
        check(&scope, "unpack(.pkg)").ty(),
        &scalar(Type::Module(ModuleType::Any)),
        "`unpack(blob)` yields an unrefined `module` value"
    );
    Ok(())
}

#[test]
fn unpack_defers_materialization_to_a_pending_handle() -> Result<(), String> {
    // §13.16: `unpack` reads no more than needed — it wraps the blob as a Pending
    // handle and does NOT decode or mount; that is deferred to apply/read.
    let d = descriptor()?;
    let (scope, env, dot) = with_blob(d.clone());
    match eval(&scope, &env, &dot, "unpack(.pkg)") {
        Cell::Scalar(Value::Module(ModuleHandle::Pending(inner))) => {
            assert_eq!(
                *inner, d,
                "the pending handle carries the source blob verbatim, undecoded"
            );
        }
        other => return Err(format!("expected a pending module value, got {other:?}")),
    }
    Ok(())
}

// ---- §13.16 lifecycle operators -------------------------------------------

/// Each operator's result type is externally deducible from §13.16: `pack` crosses
/// to a blob; `update_module` reports the decoded package identity, the same fact
/// the declarative `module.update` reports; `rollback_module` reports the point it
/// selected.
#[test]
fn the_lifecycle_operators_type_to_their_spec_results() -> Result<(), String> {
    let (scope, _env, _dot) = with_blob(descriptor()?);
    for (source, expected) in [
        ("pack(unpack(.pkg))", Type::Blob),
        ("pack(unpack(.pkg), { data: now() })", Type::Blob),
        ("update_module(unpack(.pkg), unpack(.pkg))", Type::Text),
        ("update_module(unpack(.pkg), unpack(.pkg), { migrate: \"model+data\" })", Type::Text),
        ("rollback_module(unpack(.pkg), now())", Type::Text),
        ("rollback_module(unpack(.pkg), .pkg)", Type::Text),
    ] {
        assert_eq!(check(&scope, source).ty(), &scalar(expected), "`{source}` result type");
    }
    Ok(())
}

/// Every operand shape §13.16 does not define is a LOAD error, not a runtime
/// surprise: a non-module operand, an axis the operator has no coordinate for, an
/// axis addressed by the wrong coordinate type, an unknown `migrate` spelling, and
/// a rollback coordinate that is neither an instant nor the artifact carrying it.
#[test]
fn the_operators_refuse_every_undefined_operand_shape() -> Result<(), String> {
    let (scope, _env, _dot) = with_blob(descriptor()?);
    for (source, expected) in [
        ("pack(.pkg)", "`module` value"),
        ("pack(unpack(.pkg), { snapshot: now() })", "no `snapshot` axis"),
        ("pack(unpack(.pkg), { data: \"yesterday\" })", "`data` axis is addressed by a `timestamp`"),
        ("pack(unpack(.pkg), { model: 3 })", "`model` axis is addressed by a `text` version"),
        ("update_module(unpack(.pkg))", "the module to apply onto it"),
        ("update_module(unpack(.pkg), .pkg)", "`onto` operand is a `module` value"),
        ("update_module(unpack(.pkg), unpack(.pkg), { migrate: \"data\" })", "one of model or model+data"),
        ("update_module(unpack(.pkg), unpack(.pkg), { carry: \"data\" })", "no `carry` axis"),
        ("rollback_module(unpack(.pkg))", "one retained-point coordinate"),
        ("rollback_module(unpack(.pkg), 7)", "addresses a retained point"),
    ] {
        let mut sources = SourceMap::new();
        let _ = sources.add_label("test", source);
        let rendered = check_rejects(&scope, source).render(&sources);
        assert!(
            rendered.contains(expected),
            "`{source}` must be refused naming {expected:?}, got:\n{rendered}"
        );
    }
    Ok(())
}

/// §13.16/§13.10: the three lifecycle operators are host-privileged. The pure
/// evaluator can reach no engine, so it must refuse — a fabricated blob, identity,
/// or point would report a lifecycle that never happened.
#[test]
fn the_pure_evaluator_refuses_every_host_privileged_operator() -> Result<(), String> {
    let (scope, env, dot) = with_blob(descriptor()?);
    for (source, operator) in [
        ("pack(unpack(.pkg))", "pack"),
        ("update_module(unpack(.pkg), unpack(.pkg))", "update_module"),
        ("rollback_module(unpack(.pkg), now())", "rollback_module"),
    ] {
        match try_eval(&scope, &env, &dot, source) {
            Err(EvalError::ModuleLifecycle { operator: named }) => {
                assert_eq!(named, operator, "the refusal names the operator that was reached");
            }
            other => return Err(format!("`{source}` must refuse in a pure position, got {other:?}")),
        }
    }
    Ok(())
}
