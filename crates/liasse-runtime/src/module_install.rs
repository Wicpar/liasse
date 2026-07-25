//! Lowering the §13.16 `<-` move into the §13.10 install / relocate lifecycle.
//!
//! `.modules[@id] <- unpack(@package)` writes a module VALUE into one entry of a
//! module collection. The destination is resolved by the interpreter's **ordinary**
//! collection addressing — the very same [`Interp::row_target`] that resolves
//! `.templates[@id]` — so the containing rows are whatever the containing
//! collections give, at any depth, and there is no mount path to mint.
//!
//! That is what makes a wrong destination unrepresentable rather than unlikely.
//! Previously the destination had to be translated into a module-space display path
//! that interleaved the containing-row keys, and a mistranslation installed into a
//! real, different space while every call reported success. There is no translation
//! left to get wrong: `.modules[@id]` resolves exactly as any other keyed write
//! does, under the language's own name resolution, and the resulting [`RowAddress`]
//! IS the instance's identity.
//!
//! What remains here is the classification and the two refusals that are genuinely
//! about modules: a `module` value written somewhere that is not a module
//! collection (which would stage nothing), and a non-module written into one.

use liasse_expr::check_expression;
use liasse_model::{lifecycle_arg as arg, LifecycleOp, ModuleOperator, MOVE_OPERATOR};
use liasse_store::RowAddress;
use liasse_syntax::{Arg, Expr, ExprKind};
use liasse_value::{Type, Value};

use liasse_diag::SourceId;

use crate::error::{Rejection, RejectionReason};
use crate::interp::Interp;
use crate::module_ops::module_operator;

impl Interp<'_> {
    /// Run `dest <- src` as a §13.16 install / relocate when it is one; `None` falls
    /// through to the ordinary §8.5 binding transfer.
    pub(crate) fn module_move(&mut self, dest: &Expr, src: &Expr, at: SourceId) -> Option<Result<(), Rejection>> {
        // §8.5: `m <- unpack(@pkg)` moves the handle into a LEXICAL LOCAL. That is an
        // ordinary binding transfer — no instance is carried anywhere — and it is the
        // only way to bind a module at all, since `=` copies (§8.5) and a module is
        // move-only (§13.16).
        if matches!(&dest.kind, ExprKind::Name(_)) {
            return None;
        }
        let moves_a_module = self.source_is_module(src, at) || reinstall_source(src).is_some();
        let slot = match self.module_slot(dest, at) {
            Ok(slot) => slot,
            Err(rejection) => return Some(Err(rejection)),
        };
        match (slot, moves_a_module) {
            // Neither a module-collection write nor a module value: an ordinary move.
            (None, false) => None,
            // §13.16: a module is installed by moving it INTO a module collection.
            // Any other destination stages nothing at all, so refuse by name rather
            // than commit a transition that silently did nothing.
            (None, true) => Some(Err(not_a_module_collection())),
            // A module-collection entry written with something that is not a module.
            (Some(_), false) => Some(Err(not_a_module())),
            (Some(address), true) => Some(self.install_into(address, src, at)),
        }
    }

    /// The module-collection entry `dest` addresses, if it addresses one. Resolution
    /// is the interpreter's ordinary keyed-write resolution; the only module-specific
    /// step is asking the compiled package whether that collection holds modules.
    fn module_slot(&self, dest: &Expr, at: SourceId) -> Result<Option<RowAddress>, Rejection> {
        let Some(target) = self.row_target(dest, at)? else {
            return Ok(None);
        };
        Ok(self.compiled.module_collection(&target.path).map(|_| target.address))
    }

    /// Route the resolved slot and its module value through the host-privileged
    /// lifecycle handle.
    fn install_into(&mut self, address: RowAddress, src: &Expr, at: SourceId) -> Result<(), Rejection> {
        // §13.16: `<- reinstall_module(m)` is the explicit re-admission form. The
        // operand is the instance; the operator names what the move does to it, so
        // it is unwrapped here and the operation, not the value, changes.
        let (op, value) = match reinstall_source(src) {
            Some(operand) => (LifecycleOp::Reinstall, operand),
            None => (LifecycleOp::InstallModule, src),
        };
        let current = self.current()?;
        let Value::Module(handle) = self.operand_value(value, at, &current)? else {
            return Err(Rejection::new(
                RejectionReason::TypeError,
                format!(
                    "`{MOVE_OPERATOR}` into `{}` moves a `module` value (§13.16), but the source did \
                     not evaluate to one",
                    address.render(),
                ),
            ));
        };
        let Some(lifecycle) = self.lifecycle else {
            return Err(Rejection::new(
                RejectionReason::Malformed,
                format!(
                    "`{MOVE_OPERATOR}` into `{}` carries a module instance through its lifecycle and \
                     is host-privileged (§13.16/§13.10): writing into a module collection is the \
                     host/root-scope transition's authority, and this caller is not lent it",
                    address.render(),
                ),
            ));
        };
        lifecycle.perform(op, Some(address), vec![(arg::MODULE.to_owned(), Value::Module(handle))])?;
        Ok(())
    }

    /// Whether the move's source is a `module` value (§13.16), decided by TYPE — no
    /// evaluation, so classifying a move never performs one.
    fn source_is_module(&self, src: &Expr, at: SourceId) -> bool {
        check_expression(&self.scope(), at, src)
            .is_ok_and(|typed| matches!(typed.ty().as_scalar(), Some(Type::Module(_))))
    }
}

/// The instance operand of a `reinstall_module(m)` move source (§13.16), when the
/// source is one. Recognised structurally, exactly as the other §13.16 operators
/// are, so the classification never depends on a typing pass having succeeded.
fn reinstall_source(src: &Expr) -> Option<&Expr> {
    let ExprKind::Call { callee, args } = &src.kind else { return None };
    if module_operator(callee) != Some(ModuleOperator::Reinstall) {
        return None;
    }
    match args.as_slice() {
        [Arg::Positional(operand)] => Some(operand),
        _ => None,
    }
}

/// The refusal for a `module` value moved into a destination that is not an entry
/// of a declared module collection — a bare field, an ordinary collection, a
/// multi-key selection. None of them mounts an instance, so the move would stage
/// nothing at all.
fn not_a_module_collection() -> Rejection {
    Rejection::new(
        RejectionReason::Malformed,
        format!(
            "a `module` value is installed by moving it into one entry of a module collection — a \
             map declared `{{ $key: text, $value: module }}` — and `{MOVE_OPERATOR}` writes \
             `.<collection>[<name>]` (§13.16/§13.2). This destination is not one, so the move \
             would stage nothing; refused."
        ),
    )
}

/// The refusal for a non-module value written into a module-collection entry.
fn not_a_module() -> Rejection {
    Rejection::new(
        RejectionReason::TypeError,
        "a module collection's entries are `module` values (§13.2); the moved source is not one — \
         build one with `unpack(@package)`."
            .to_owned(),
    )
}
