//! Typing of the keyring public version selectors `.$current`, `.$accepted`,
//! `.$public`, `.$versions` (§17.2), and the dispatch of every argument-less
//! structural member selector `.base.$name` it shares with the §14 temporal
//! `.$all`.
//!
//! A keyring exposes its managed versions as a view of version-metadata rows
//! (§17.2). The four selectors name lifecycle subsets of that view: `.$current`
//! is the single active version (§17.3 pins at most one active version, so it
//! types as one row), while `.$accepted`, `.$public`, and `.$versions` are
//! version streams. As with the temporal selectors this layer types the
//! selector over any view and leaves *which* view is a genuine keyring — and how
//! each subset is computed — to the model and the environment's keyring index;
//! keeping that here would duplicate lifecycle state the checker cannot see.

use liasse_syntax::Expr;

use crate::check::Checker;
use crate::env::KeyringSelector;
use crate::ty::ExprType;
use crate::typed::{TypedExpr, TypedKind};

impl Checker<'_> {
    /// Dispatch an argument-less structural member selector `.base.$name`. `.$all`
    /// is the §14.2 temporal selector; `.$current`/`.$accepted`/`.$public`/
    /// `.$versions` are the §17.2 keyring version selectors. Any other `.$name`
    /// is not a member selector.
    pub(crate) fn check_structural_selector(
        &mut self,
        expr: &Expr,
        base: &Expr,
        selector: &str,
    ) -> Option<TypedExpr> {
        use crate::typed::BlobMember;
        let keyring = match selector {
            "all" => return self.check_temporal_all(expr, base),
            "key" => return self.check_key_selector(expr, base),
            "current" => KeyringSelector::Current,
            "accepted" => KeyringSelector::Accepted,
            "public" => KeyringSelector::Public,
            "versions" => KeyringSelector::Versions,
            "sha512" => return self.check_blob_selector(expr, base, BlobMember::Sha512),
            "bytes" => return self.check_blob_selector(expr, base, BlobMember::Bytes),
            "media" => return self.check_blob_selector(expr, base, BlobMember::Media),
            "name" => return self.check_blob_selector(expr, base, BlobMember::Name),
            other => {
                // §14.4: a source-backed bucket row exposes its structural bindings
                // (`$source`/`$from`/`$until`/`$index`) as readable members, so
                // `p.$source.account` and `pool.$until` resolve off the row.
                if let Some(typed) = self.check_row_structural_member(expr, base, other) {
                    return Some(typed);
                }
                return self.error(
                    expr,
                    format!(
                        "`.${other}` is not a selector (row identity `.$key`, §6.3; temporal \
                         `.$all`, §14.2; keyring \
                         `.$current`/`.$accepted`/`.$public`/`.$versions`, §17.2; blob \
                         `.$sha512`/`.$bytes`/`.$media`/`.$name`, §18.1)"
                    ),
                );
            }
        };
        self.check_keyring_selector(expr, base, keyring)
    }

    /// `base.$name` reading a source-backed bucket row's structural binding
    /// (§14.4): when `base` types as a row exposing structural `name`, the access
    /// resolves to that binding's value. The derived row carries it as a `$name`
    /// cell, so this lowers to an ordinary field read of that cell.
    fn check_row_structural_member(&mut self, expr: &Expr, base: &Expr, name: &str) -> Option<TypedExpr> {
        let typed = self.check(base)?;
        let structural = match typed.ty() {
            ExprType::Row(row) | ExprType::View(row) => row.structural(name)?.clone(),
            _ => return None,
        };
        Some(TypedExpr::new(
            expr.span,
            structural,
            TypedKind::Field { base: Box::new(typed), name: format!("${name}") },
        ))
    }

    /// `base.$key` (§6.3): the identity key value of a bound keyed row. The base
    /// is a single row (`$actor`, a `login`/`session` binding, a keyed selection),
    /// and the result is that row's key value — the same value a key selector
    /// `collection[key]` matches against. A keyless row (a static struct or a
    /// projected group without a synthetic `$key`) has no identity key, so `.$key`
    /// on it is a static error.
    fn check_key_selector(&mut self, expr: &Expr, base: &Expr) -> Option<TypedExpr> {
        let base = self.check(base)?;
        let key = match base.ty() {
            ExprType::Row(row) => row.key().cloned(),
            other => {
                return self.error(
                    expr,
                    format!("`.$key` reads the identity key of a row, not a {}", other.describe()),
                );
            }
        };
        match key {
            Some(key) => Some(TypedExpr::new(
                expr.span,
                key,
                TypedKind::Key(Box::new(base)),
            )),
            None => self.error(expr, "`.$key` needs a keyed row, but this row has no identity key"),
        }
    }

    /// A keyring public version selector (§17.2) over a keyring's version view.
    /// `.$current` yields the single active version (one row, §17.3); the rest
    /// yield a stream of version rows. The version row shape is the base view's,
    /// so a selector composes with ordinary projection and field access.
    fn check_keyring_selector(
        &mut self,
        expr: &Expr,
        base: &Expr,
        selector: KeyringSelector,
    ) -> Option<TypedExpr> {
        let (base, row) = self.selector_base(base, "a keyring selector")?;
        let ty = match selector {
            KeyringSelector::Current => ExprType::Row(row),
            KeyringSelector::Accepted | KeyringSelector::Public | KeyringSelector::Versions => {
                ExprType::View(row)
            }
        };
        Some(TypedExpr::new(
            expr.span,
            ty,
            TypedKind::Keyring {
                base: Box::new(base),
                selector,
            },
        ))
    }
}
