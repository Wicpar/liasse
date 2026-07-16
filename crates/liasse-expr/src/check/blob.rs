//! Typing of the `blob` descriptor member selectors `.$sha512`, `.$bytes`,
//! `.$media`, `.$name` (§18.1).
//!
//! §18.1: a `blob` field holds a descriptor for binary content whose members an
//! expression reads directly. `$sha512` is the content hash as its canonical
//! lowercase-hexadecimal `text`, `$bytes` the non-negative `int` byte count,
//! `$media` the canonical media type (`text`), and `$name` the optional file
//! name (`optional<text>`, the one member §18.1 marks optional). The complete
//! descriptor is the application value, so a selector composes with ordinary
//! field access and projection.

use liasse_syntax::Expr;
use liasse_value::Type;

use crate::check::Checker;
use crate::ty::ExprType;
use crate::typed::{BlobMember, TypedExpr, TypedKind};

impl Checker<'_> {
    /// A `blob` descriptor member selector (§18.1) over a `blob` value. The base
    /// must be a `blob`; each member reads its typed metadata off the descriptor.
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
                    "`.${name}` reads a blob descriptor member, but this base is a {} (§18.1)",
                    base.ty().describe()
                ),
            );
        }
        let ty = match member {
            BlobMember::Sha512 | BlobMember::Media => Type::Text,
            BlobMember::Bytes => Type::Int,
            BlobMember::Name => Type::Optional(Box::new(Type::Text)),
        };
        Some(TypedExpr::new(
            expr.span,
            ExprType::scalar(ty),
            TypedKind::BlobMember {
                base: Box::new(base),
                member,
            },
        ))
    }
}

/// The `$`-member spelling of a blob descriptor member, for diagnostics.
fn blob_member_name(member: BlobMember) -> &'static str {
    match member {
        BlobMember::Sha512 => "sha512",
        BlobMember::Bytes => "bytes",
        BlobMember::Media => "media",
        BlobMember::Name => "name",
    }
}
