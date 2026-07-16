#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Blob descriptor member selectors `.$sha512`, `.$bytes`, `.$media`, `.$name`
//! (§18.1).
//!
//! §18.1: a `blob` field holds a descriptor whose members an expression reads
//! directly. `$sha512` is the content hash as canonical lowercase-hex `text`,
//! `$bytes` the non-negative `int` byte count, `$media` the canonical media type
//! (`text`), and `$name` the optional file name (`optional<text>`). Expected
//! read values are the descriptor's own components, so nothing is tautological.

mod common;

use common::{check, check_rejects, keyless_row, row_type, scalar, scell, view, FixedEnv, FixedScope};
use liasse_expr::{Cell, ExprType};
use liasse_value::{BlobDescriptor, Integer, MediaType, Sha512, Text, Type, Value};

/// The 128-hex canonical text of a fixed SHA-512 (64 `0xab` bytes).
fn hash_hex() -> String {
    "ab".repeat(64)
}

/// A descriptor for 184320 bytes of `application/pdf` named `invoice-113.pdf`
/// (the §18.1 example shape), and one without a name.
fn descriptor(name: Option<&str>) -> Value {
    Value::Blob(Box::new(BlobDescriptor::new(
        Sha512::parse(&hash_hex()).expect("hash"),
        184_320,
        MediaType::new("application/pdf"),
        name.map(str::to_owned),
    )))
}

/// A scope whose current row exposes a `file` blob field.
fn scope() -> FixedScope {
    let root = row_type(vec![("file", scalar(Type::Blob))], None);
    FixedScope::new(ExprType::Row(root))
}

/// A `.` row carrying a blob `file` cell.
fn world(name: Option<&str>) -> Cell {
    Cell::Row(Box::new(keyless_row(0, vec![("file", scell(descriptor(name)))])))
}

/// §18.1 typing: `$bytes` is `int`, `$sha512`/`$media` are `text`, `$name` is
/// `optional<text>`.
#[test]
fn descriptor_members_type_as_declared() {
    let scope = scope();
    assert_eq!(check(&scope, ".file.$bytes").ty(), &ExprType::scalar(Type::Int));
    assert_eq!(check(&scope, ".file.$sha512").ty(), &ExprType::scalar(Type::Text));
    assert_eq!(check(&scope, ".file.$media").ty(), &ExprType::scalar(Type::Text));
    assert_eq!(
        check(&scope, ".file.$name").ty(),
        &ExprType::scalar(Type::Optional(Box::new(Type::Text))),
    );
}

/// §18.1: `$bytes` reads the descriptor's non-negative byte count as `int`.
#[test]
fn bytes_reads_the_byte_count() {
    let result = common::eval(&scope(), &FixedEnv::new(row(None)), &world(Some("invoice-113.pdf")), ".file.$bytes");
    assert_eq!(result.as_scalar(), Some(&Value::Int(Integer::from(184_320))));
}

/// §18.1: `$sha512` reads the content hash as its canonical lowercase-hex text.
#[test]
fn sha512_reads_canonical_hex() {
    let result = common::eval(&scope(), &FixedEnv::new(row(None)), &world(None), ".file.$sha512");
    assert_eq!(result.as_scalar(), Some(&Value::Text(Text::new(hash_hex()))));
}

/// §18.1: `$media` reads the canonical media type.
#[test]
fn media_reads_the_media_type() {
    let result = common::eval(&scope(), &FixedEnv::new(row(None)), &world(None), ".file.$media");
    assert_eq!(result.as_scalar(), Some(&Value::Text(Text::new("application/pdf"))));
}

/// §18.1: `$name` reads the present optional file name.
#[test]
fn name_reads_present_file_name() {
    let result = common::eval(&scope(), &FixedEnv::new(row(None)), &world(Some("invoice-113.pdf")), ".file.$name");
    assert_eq!(result.as_scalar(), Some(&Value::Text(Text::new("invoice-113.pdf"))));
}

/// §18.1: `$name` is the one optional member, so an absent name reads `none`.
#[test]
fn name_absent_reads_none() {
    let result = common::eval(&scope(), &FixedEnv::new(row(None)), &world(None), ".file.$name");
    assert!(matches!(result, Cell::Scalar(Value::None)));
}

/// A blob descriptor selector applies only to a `blob`; over a non-blob it is a
/// static type error naming §18.1.
#[test]
fn blob_selector_over_non_blob_rejects() {
    let scope = FixedScope::new(ExprType::scalar(Type::Int));
    let diags = check_rejects(&scope, "1.$bytes");
    assert!(diags.iter().any(|d| {
        let m = d.message();
        m.contains("blob descriptor member") && m.contains("18.1")
    }));
}

/// The `$bytes`/`$sha512`/`$media`/`$name` names now appear in the structural
/// selector diagnostic so an unknown `.$name`-style selector still lists them.
#[test]
fn unknown_selector_lists_blob_members() {
    let scope = FixedScope::new(view(row_type(vec![], None)));
    let diags = check_rejects(&scope, ".$bogus");
    assert!(diags.iter().any(|d| d.message().contains("$sha512")));
}

/// A `.` row with no blob cell; the env root is unused by these field reads but
/// required by the evaluator contract.
fn row(_name: Option<&str>) -> liasse_expr::Row {
    keyless_row(0, vec![])
}
