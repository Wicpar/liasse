//! Typing of the `blob` descriptor member selectors `.$sha512`, `.$bytes`,
//! `.$media`, `.$name` (§18.1) and the placement member selectors `.$satisfied`,
//! `.$stored`, `.$surplus` (§18.5).
//!
//! §18.1: a `blob` field holds a descriptor for binary content whose metadata an
//! expression reads directly. `$sha512` is the content hash as its canonical
//! lowercase-hexadecimal `text`, `$bytes` the non-negative `int` byte count,
//! `$media` the canonical media type (`text`), and `$name` the optional file
//! name (`optional<text>`, the one member §18.1 marks optional).
//!
//! §18.5: a committed blob occurrence additionally exposes its logical placement
//! state. `$satisfied` is a `bool` — whether the current placement policy holds
//! over the verified stores — while `$stored` and `$surplus` are views of keyed
//! store-identity rows (the verified stores, and the verified copies outside the
//! currently required policy). Each store-identity row carries the store's `id`
//! `text` as both its field and its key, so `.file.$stored { id }` projects the
//! identity and `/stores['id'] in .file.$stored` tests membership (§18.11).
//!
//! The complete descriptor is the application value, so every selector composes
//! with ordinary field access and projection.

use liasse_syntax::Expr;
use liasse_value::Type;

use crate::check::Checker;
use crate::ty::{ExprType, RowType};
use crate::typed::{BlobMember, TypedExpr, TypedKind};

impl Checker<'_> {
    /// A `blob` descriptor (§18.1) or placement (§18.5) member selector over a
    /// `blob` value. The base must be a `blob`; a metadata member reads its typed
    /// value off the descriptor, and a placement member yields the §18.5 logical
    /// observation at its typed shape (`$satisfied` a `bool`, `$stored`/`$surplus`
    /// a view of store-identity rows).
    pub(crate) fn check_blob_selector(
        &mut self,
        expr: &Expr,
        base: &Expr,
        member: BlobMember,
    ) -> Option<TypedExpr> {
        let base = self.check(base)?;
        if !matches!(base.ty().as_scalar(), Some(Type::Blob)) {
            let name = blob_member_name(member);
            return self.error(
                expr,
                format!(
                    "`.${name}` reads a blob descriptor member, but this base is a {} (§18.1, §18.5)",
                    base.ty().describe()
                ),
            );
        }
        let ty = match member {
            BlobMember::Sha512 | BlobMember::Media => ExprType::scalar(Type::Text),
            BlobMember::Bytes => ExprType::scalar(Type::Int),
            BlobMember::Name => ExprType::scalar(Type::Optional(Box::new(Type::Text))),
            BlobMember::Satisfied => ExprType::scalar(Type::Bool),
            BlobMember::Stored | BlobMember::Surplus => ExprType::View(store_identity_row()),
        };
        Some(TypedExpr::new(
            expr.span,
            ty,
            TypedKind::BlobMember {
                base: Box::new(base),
                member,
            },
        ))
    }
}

/// The row shape of a `$stored`/`$surplus` store-identity view (§18.5): one
/// visible `id` `text` field, keyed by that same identity, so the view projects
/// as `{ id }` and a `/stores['id']` row tests membership by key (§18.11).
fn store_identity_row() -> RowType {
    RowType::new(
        [("id".to_owned(), ExprType::scalar(Type::Text))],
        Some(ExprType::scalar(Type::Text)),
    )
}

/// The `$`-member spelling of a blob descriptor or placement member, for
/// diagnostics.
fn blob_member_name(member: BlobMember) -> &'static str {
    match member {
        BlobMember::Sha512 => "sha512",
        BlobMember::Bytes => "bytes",
        BlobMember::Media => "media",
        BlobMember::Name => "name",
        BlobMember::Satisfied => "satisfied",
        BlobMember::Stored => "stored",
        BlobMember::Surplus => "surplus",
    }
}
