//! The blob boundary (§13.16): `unpack(blob)` reads a `.liasse` blob into a
//! move-only `module` value, with materialization DEFERRED — no decode or mount
//! happens at `unpack`; the value carries the source blob as a `Pending` handle.
//!
//! Written without `unwrap`/`expect`/`panic!` so the workspace deny-lints hold:
//! fallible construction threads through `Result` + `?`.

mod common;

use common::{FixedEnv, FixedScope, check, eval, keyless_row, row_type, scalar, scell};
use liasse_expr::{Cell, ExprType};
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
