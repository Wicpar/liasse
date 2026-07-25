//! Lowering the §13.16 `<-` move into the §13.10 install/relocate lifecycle.
//!
//! `.modules[@id] <- unpack(@package)` writes a module VALUE into a slot of a
//! `$modules` space. The host addresses instances by the space's **mount path**
//! (`/companies/acme/modules`), which interleaves the containing-row keys, so this
//! module's whole job is to turn a *written* destination into exactly the right
//! mount — or to refuse.
//!
//! # Why it cannot pick the wrong space
//!
//! Installing into the wrong space would produce a module state that is wrong while
//! every call reports success, so the resolution is built to make that
//! unrepresentable rather than unlikely:
//!
//! 1. **The candidate set is closed.** A destination contributes at most two
//!    readings — the package root's space and the receiver row's — and each survives
//!    only if the compiled package DECLARES a `$modules` node at that declaration
//!    path. An undeclared path resolves to nothing; it is never invented.
//! 2. **Two readings are an ambiguity, not a precedence.** When both survive, the
//!    move is REFUSED naming both mounts and the two unambiguous spellings, because
//!    nothing in the statement says which was meant.
//! 3. **The path is built, not concatenated.** [`ModuleSpace::mounted_at`] renders
//!    the containing row through the D.3 canonical path, so a key holding `/`
//!    escapes to `%2F` and cannot forge a path level.
//! 4. **The result is re-derived and compared.** The minted mount's own
//!    `declaration_path()` — recomputed by the same parse the module host applies —
//!    must equal the declaration path that was resolved, and its containing-row
//!    steps must have the arity of the row that was addressed. A mismatch is a
//!    refusal, never an install.
//! 5. **The host re-checks the row.** `check_containing_row` resolves the minted
//!    path against live root state, so a mount naming a row that is not there
//!    rejects the whole transition.
//!
//! Both shapes that would otherwise pass silently are refusals here too: a `module`
//! moved somewhere that is not a declared slot (which would stage nothing), and a
//! non-module moved into one.

use liasse_expr::check_expression;
use liasse_model::{lifecycle_arg as arg, LifecycleOp, MOVE_OPERATOR};
use liasse_syntax::{Expr, ExprKind, Selector};
use liasse_value::{Text, Type, Value};

use liasse_diag::SourceId;

use crate::error::{Rejection, RejectionReason};
use crate::interp::{Interp, RowTarget};
use crate::modules::ModuleSpace;

/// A written `<-` destination with the SHAPE of a module-space slot:
/// `<container>.<declaration>[<key>]` (§13.16 `.modules[@id]`). Parsing is purely
/// syntactic — whether the declaration names a declared `$modules` space is decided
/// against the compiled package, not here.
struct SlotSyntax<'a> {
    /// What the `$modules` node hangs off: `.` (the receiver, or the package root
    /// in a root program), `/` (the package root), or a row selector.
    container: &'a Expr,
    /// The `$modules` node's declaration name.
    declaration: &'a str,
    /// The key expression selecting the slot — the instance name (§13.3).
    key: &'a Expr,
}

impl<'a> SlotSyntax<'a> {
    fn parse(dest: &'a Expr) -> Option<Self> {
        let ExprKind::Select { base, selector: Selector::Keys(keys) } = &dest.kind else { return None };
        let [key] = keys.as_slice() else { return None };
        let ExprKind::Field { base: container, member } = &base.kind else { return None };
        if member.structural {
            return None;
        }
        Some(Self { container, declaration: member.text.as_str(), key })
    }

    /// How the destination reads in the source, so a diagnostic quotes the author's
    /// own spelling rather than an internal path.
    fn render(&self) -> String {
        let base = match &self.container.kind {
            ExprKind::Current => ".",
            ExprKind::Root => "/",
            _ => ".….",
        };
        format!("{base}{}[…]", self.declaration)
    }
}

/// One reading of a written destination: the `$modules` declaration path it names,
/// and the containing row that path hangs off (`None` = the package root, which is
/// always live).
struct Candidate {
    declaration_path: Vec<String>,
    containing: Option<RowTarget>,
}

impl Candidate {
    /// The mount this reading addresses, proven to read back as the declaration path
    /// it came from. Every wrong-space refusal funnels through here.
    fn mount(&self) -> Result<ModuleSpace, Rejection> {
        let declaration = self.declaration_path.last().ok_or_else(|| {
            Rejection::new(RejectionReason::Malformed, "a `$modules` declaration path names at least one segment")
        })?;
        let space = ModuleSpace::mounted_at(self.containing.as_ref().map(|row| &row.address), declaration)
            .map_err(|error| Rejection::new(RejectionReason::Malformed, error.to_string()))?;
        // Re-derive both halves of the mount from the rendered path — by the SAME
        // parse the module host applies — and require them to agree with what was
        // resolved. This is what makes a wrong space a refusal rather than a silent
        // install: a display path that does not read back as this declaration under
        // a row of this depth is not this space.
        let depth = self.containing.as_ref().map_or(0, |row| row.address.depth());
        if space.declaration_path() != self.declaration_path
            || space.containing_row_steps().is_none_or(|steps| steps.len() != depth)
        {
            return Err(Rejection::new(
                RejectionReason::Malformed,
                format!(
                    "the mount `{}` does not read back as the declared `$modules` node `{}` under \
                     its containing row (§13.2), so it is refused rather than installed into a \
                     space it may not name",
                    space.as_str(),
                    self.declaration_path.join("."),
                ),
            ));
        }
        Ok(space)
    }
}

impl Interp<'_> {
    /// Run `dest <- src` as a §13.16 install/relocate when it is one; `None` falls
    /// through to the ordinary §8.5 binding transfer.
    pub(crate) fn module_move(&mut self, dest: &Expr, src: &Expr, at: SourceId) -> Option<Result<(), Rejection>> {
        // §8.5: `m <- unpack(@pkg)` moves the handle into a LEXICAL LOCAL. That is an
        // ordinary binding transfer — no instance is carried anywhere — and it is the
        // only way to bind a module at all, since `=` copies (§8.5) and a module is
        // move-only (§13.16).
        if matches!(&dest.kind, ExprKind::Name(_)) {
            return None;
        }
        let slot = SlotSyntax::parse(dest);
        let moves_a_module = self.source_is_module(src, at);
        let candidates = match slot.as_ref().map(|slot| self.space_candidates(slot, at)) {
            Some(Ok(candidates)) => candidates,
            Some(Err(rejection)) => return Some(Err(rejection)),
            None => Vec::new(),
        };
        match (candidates.as_slice(), moves_a_module) {
            // Neither a module-space write nor a module value: an ordinary move.
            ([], false) => None,
            // §13.16: a module is installed by moving it INTO a module space. Any
            // other destination stages nothing at all, so refuse by name rather than
            // commit a transition that silently did nothing.
            ([], true) => Some(Err(self.no_such_space(slot.as_ref()))),
            // A declared slot written with something that is not a module value.
            ([_], false) => Some(Err(not_a_module(slot.as_ref()))),
            ([candidate], true) => Some(self.install_into(candidate, slot.as_ref(), src, at)),
            // Two declared spaces of that name are reachable from this destination
            // and the statement does not say which. Refuse; never order them.
            (_, _) => Some(Err(ambiguous(&candidates, slot.as_ref()))),
        }
    }

    /// Resolve the slot and route the move through the host-privileged handle.
    fn install_into(
        &mut self,
        candidate: &Candidate,
        slot: Option<&SlotSyntax<'_>>,
        src: &Expr,
        at: SourceId,
    ) -> Result<(), Rejection> {
        let Some(slot) = slot else {
            return Err(Rejection::new(RejectionReason::Malformed, "a module move resolves one module space"));
        };
        let space = candidate.mount()?;
        let current = self.current()?;
        let name = instance_name(self.scalar_value(slot.key, at, &current)?)?;
        let Value::Module(handle) = self.scalar_value(src, at, &current)? else {
            return Err(Rejection::new(
                RejectionReason::TypeError,
                format!(
                    "`{MOVE_OPERATOR}` into `{}` moves a `module` value (§13.16), but the source did \
                     not evaluate to one",
                    space.as_str(),
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
                    space.as_str()
                ),
            ));
        };
        lifecycle.perform(
            LifecycleOp::InstallModule,
            vec![
                ("space".to_owned(), Value::Text(Text::new(space.as_str()))),
                ("name".to_owned(), Value::Text(Text::new(name))),
                (arg::MODULE.to_owned(), Value::Module(handle)),
            ],
        )?;
        Ok(())
    }

    /// Whether the move's source is a `module` value (§13.16), decided by TYPE — no
    /// evaluation, so classifying a move never performs one.
    fn source_is_module(&self, src: &Expr, at: SourceId) -> bool {
        check_expression(&self.scope(), at, src)
            .is_ok_and(|typed| matches!(typed.ty().as_scalar(), Some(Type::Module(_))))
    }

    /// Every reading of `slot`'s destination the compiled package DECLARES as a
    /// `$modules` space.
    ///
    /// `.name` is the one spelling with two readings — the package root's space and
    /// the receiver row's — and both are produced here, so a package declaring both
    /// yields an ambiguity the caller refuses. `/name` and an explicit row selector
    /// each have exactly one reading by construction.
    fn space_candidates(&self, slot: &SlotSyntax<'_>, at: SourceId) -> Result<Vec<Candidate>, Rejection> {
        let containers: Vec<Option<RowTarget>> = match &slot.container.kind {
            ExprKind::Current => std::iter::once(None).chain(self.receiver.clone().map(Some)).collect(),
            ExprKind::Root => vec![None],
            _ => self.row_target(slot.container, at)?.map(Some).into_iter().collect(),
        };
        Ok(containers.into_iter().filter_map(|containing| self.candidate_at(containing, slot.declaration)).collect())
    }

    /// The candidate for `declaration` under `containing`, if the package declares a
    /// `$modules` node at that declaration path.
    fn candidate_at(&self, containing: Option<RowTarget>, declaration: &str) -> Option<Candidate> {
        let mut declaration_path = containing.as_ref().map(|row| row.path.clone()).unwrap_or_default();
        declaration_path.push(declaration.to_owned());
        self.compiled.module_space(&declaration_path)?;
        Some(Candidate { declaration_path, containing })
    }

    /// The refusal for a module value moved somewhere that is not a declared
    /// `$modules` slot. It names the destination as written and every space of that
    /// declaration name the package DOES declare, so the fix is their difference.
    fn no_such_space(&self, slot: Option<&SlotSyntax<'_>>) -> Rejection {
        let Some(slot) = slot else {
            return Rejection::new(
                RejectionReason::Malformed,
                format!(
                    "a `module` value is installed by moving it into a slot of a declared \
                     `$modules` space — `{MOVE_OPERATOR}` writes `.<space>[<name>]` (§13.16). This \
                     destination is not a keyed slot, so the move would stage nothing; refused."
                ),
            );
        };
        let declared: Vec<String> =
            self.compiled.module_spaces_named(slot.declaration).map(|path| path.join(".")).collect();
        let where_declared = if declared.is_empty() {
            format!("this package declares no `$modules` space named `{}`", slot.declaration)
        } else {
            format!("this package declares `{}` as a `$modules` space at {}", slot.declaration, declared.join(", "))
        };
        Rejection::new(
            RejectionReason::Malformed,
            format!(
                "`{}` does not resolve to a declared `$modules` space here, so moving a `module` \
                 into it would stage nothing (§13.16/§13.2). {where_declared}.",
                slot.render(),
            ),
        )
    }
}

/// §13.3: an instance name is "a non-empty text value that forms the local component
/// of instance identity". A slot addressed by anything else names no instance, so it
/// is refused rather than rendered into one.
fn instance_name(key: Value) -> Result<String, Rejection> {
    match key {
        Value::Text(text) if !text.as_str().is_empty() => Ok(text.as_str().to_owned()),
        Value::Text(_) => Err(Rejection::new(
            RejectionReason::Malformed,
            "a module instance name is a NON-EMPTY text value (§13.3); the empty name addresses no slot",
        )),
        _ => Err(Rejection::new(
            RejectionReason::TypeError,
            "a module instance name is a `text` value (§13.3); this slot key is not one",
        )),
    }
}

/// The refusal for a non-module value written into a module slot.
fn not_a_module(slot: Option<&SlotSyntax<'_>>) -> Rejection {
    Rejection::new(
        RejectionReason::TypeError,
        format!(
            "`{}` is a `$modules` space, whose members are `module` values (§13.16); the moved \
             source is not one — build one with `unpack(@package)`.",
            slot.map_or_else(|| ".…[…]".to_owned(), SlotSyntax::render),
        ),
    )
}

/// The refusal for a destination that names two declared spaces.
fn ambiguous(candidates: &[Candidate], slot: Option<&SlotSyntax<'_>>) -> Rejection {
    let mounts: Vec<String> = candidates
        .iter()
        .map(|candidate| {
            candidate.mount().map_or_else(|_| candidate.declaration_path.join("."), |space| space.as_str().to_owned())
        })
        .collect();
    let declaration = slot.map_or("…", |slot| slot.declaration);
    Rejection::new(
        RejectionReason::Malformed,
        format!(
            "`{}` names more than one declared `$modules` space here — {} — and nothing in the \
             statement says which (§13.2). Refused rather than installed into a guess: address \
             the package-root space as `/{declaration}[…]`, or the row-scoped one through its \
             containing row (`.<collection>[<key>].{declaration}[…]`).",
            slot.map_or_else(|| ".…[…]".to_owned(), SlotSyntax::render),
            mounts.join(" and "),
        ),
    )
}
