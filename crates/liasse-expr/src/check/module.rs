//! Typing of the §13.16 module blob boundary and lifecycle operators.
//!
//! `unpack` is the one operator with no host authority — it wraps a blob
//! descriptor in a deferred handle — so it types and evaluates in place. `pack`,
//! `update_module` and `rollback_module` carry a module instance through its
//! §13.10 lifecycle: they are typed here and REFUSED by the pure evaluator, so a
//! view or a pure-expression position can never reach them. The runtime routes
//! them to the host-privileged handle instead.
//!
//! Every module operand is BORROWED, per §13.16: "reading it, dispatching into
//! it, or passing it as an argument borrows it without transferring ownership".
//! Only `<-`/`->` and a removal transfer or drop a handle, so nothing here needs
//! a move-tracker interaction.

use liasse_syntax::{Arg, BlockMemberKind, Expr, ExprKind};
use liasse_value::{ModuleType, Type};

use crate::check::Checker;
use crate::lifecycle::{arg, MigrateAxis, ModuleOperator};
use crate::ty::ExprType;
use crate::typed::{BuiltinFn, TypedExpr, TypedKind};

/// The `{ name: value }` axis object a §13.16 operator's trailing argument
/// carries, already split into its members. Parsed once at the boundary so the
/// per-operator checks read named axes rather than re-walking the AST.
struct AxisObject<'a> {
    members: Vec<(&'a str, &'a Expr)>,
}

impl<'a> AxisObject<'a> {
    /// Read an operator's trailing argument as an axis object. A `{ … }` literal
    /// and a `name: value` argument list are the same thing here (both spellings
    /// appear in §13.16's examples), so both parse to the same members. Any other
    /// member form — a projection directive, a bare shorthand, a patch assignment —
    /// yields `None`: an axis the checker cannot name is one the runtime would
    /// silently drop.
    fn parse(args: &'a [Arg]) -> Option<Self> {
        let mut members = Vec::new();
        for entry in args {
            match entry {
                Arg::Named { name, value } => members.push((name.text.as_str(), value)),
                Arg::Positional(Expr { kind: ExprKind::Object(object), .. }) => {
                    for member in object {
                        let BlockMemberKind::Named { name, value: Some(value) } = &member.kind else {
                            return None;
                        };
                        members.push((name.text.as_str(), value));
                    }
                }
                Arg::Positional(_) => return None,
            }
        }
        Some(Self { members })
    }

    /// The three `pack` axes in the fixed order the runtime reads them, each with
    /// the coordinate type §13.16 addresses it by: the definition by **version**,
    /// the state by a **point in time**, the history by a **time range**.
    const PACK: [(&'static str, AxisCoordinate); 3] = [
        (arg::MODEL, AxisCoordinate::Version),
        (arg::DATA, AxisCoordinate::Instant),
        (arg::HISTORY, AxisCoordinate::Range),
    ];

    fn get(&self, name: &str) -> Option<&'a Expr> {
        self.members.iter().find(|(key, _)| *key == name).map(|(_, value)| *value)
    }

    /// The first member whose name is not in `accepted` — the unsupported axis a
    /// silent drop would hide.
    fn unknown(&self, accepted: &[&str]) -> Option<(&'a str, &'a Expr)> {
        self.members.iter().copied().find(|(name, _)| !accepted.contains(name))
    }
}

/// The coordinate an axis is addressed by (§13.16). Kept as a type rather than a
/// bare [`Type`] because an instant admits any declared precision — the axis names
/// a position on the timeline, not a storage width.
#[derive(Debug, Clone, Copy)]
enum AxisCoordinate {
    /// A package version, spelled as text (`"1.2.0"`).
    Version,
    /// A point in time, at any precision.
    Instant,
    /// A calendar period bounding the span carried along.
    Range,
}

impl AxisCoordinate {
    /// Whether a checked operand's scalar type addresses this coordinate.
    fn admits(self, ty: Option<&Type>) -> bool {
        matches!(
            (self, ty),
            (Self::Version, Some(Type::Text))
                | (Self::Instant, Some(Type::Timestamp(_)))
                | (Self::Range, Some(Type::Period))
        )
    }

    /// How the coordinate is named in a diagnostic.
    fn describe(self) -> &'static str {
        match self {
            Self::Version => "`text` version",
            Self::Instant => "`timestamp`",
            Self::Range => "`period` range",
        }
    }
}

impl Checker<'_> {
    /// Type a §13.16 module-value operator call (`pack`, `update_module`,
    /// `rollback_module`). Returns `None` (with a diagnostic) for every shape the
    /// operator does not accept — the operators are host-privileged and destructive,
    /// so an argument the checker cannot account for is refused at load rather than
    /// carried to a runtime that would have to guess.
    pub(crate) fn check_module_operator(
        &mut self,
        expr: &Expr,
        operator: ModuleOperator,
        args: &[Arg],
    ) -> Option<TypedExpr> {
        match operator {
            ModuleOperator::Pack => self.check_pack(expr, args),
            ModuleOperator::UpdateModule => self.check_update_module(expr, args),
            ModuleOperator::Rollback => self.check_rollback_module(expr, args),
        }
    }

    /// `unpack(blob)` (§13.16): read a `.liasse` blob into a move-only `module`
    /// value, materialization DEFERRED to when the value is applied or read. Takes
    /// one positional `blob` argument and yields an unrefined `module`.
    pub(crate) fn check_unpack(&mut self, expr: &Expr, args: &[Arg]) -> Option<TypedExpr> {
        let value = match args {
            [Arg::Positional(value)] => value,
            _ => return self.error(expr, "`unpack` takes one `blob` argument (§13.16)"),
        };
        let typed = self.check(value)?;
        if typed.ty().as_scalar() != Some(&Type::Blob) {
            return self.error(
                value,
                format!("`unpack` reads a `blob` into a module, but a {} was given", typed.ty().describe()),
            );
        }
        Some(TypedExpr::new(
            expr.span,
            ExprType::scalar(Type::Module(ModuleType::Any)),
            TypedKind::Builtin { func: BuiltinFn::Unpack, args: vec![typed] },
        ))
    }

    /// `pack(m, { model?: version, data?: instant, history?: range })` → `blob`
    /// (§13.16). Each axis is optional and addresses its own coordinate on the
    /// timeline; the operands are typed here and their *reachability* (a retained
    /// point, a retained version) is the runtime's, since only the runtime knows
    /// what the instance retains.
    fn check_pack(&mut self, expr: &Expr, args: &[Arg]) -> Option<TypedExpr> {
        let (module, rest) = self.module_operand(expr, args, ModuleOperator::Pack)?;
        let mut typed = vec![module];
        if !rest.is_empty() {
            let axes = self.axis_object(expr, rest, ModuleOperator::Pack)?;
            self.reject_unknown_axis(
                &axes,
                &[arg::MODEL, arg::DATA, arg::HISTORY],
                ModuleOperator::Pack,
            )?;
            // Each axis is carried to the runtime in a fixed order, so the host
            // reads them positionally without re-deriving the names. An absent axis
            // is `none` — "the current value" (§13.16).
            for (name, coordinate) in AxisObject::PACK {
                typed.push(self.axis_operand(expr, &axes, name, coordinate)?);
            }
        } else {
            typed.extend(std::iter::repeat_with(|| TypedExpr::absent(expr.span)).take(3));
        }
        Some(TypedExpr::new(
            expr.span,
            ExprType::scalar(Type::Blob),
            TypedKind::Builtin { func: BuiltinFn::Pack, args: typed },
        ))
    }

    /// `update_module(m, u, { migrate })` (§13.16) → the decoded package identity,
    /// the same fact `module.update` reports. `migrate` is a literal axis spelling
    /// (`model` / `model+data`), checked here so an unknown spelling is a load
    /// error rather than a runtime surprise.
    fn check_update_module(&mut self, expr: &Expr, args: &[Arg]) -> Option<TypedExpr> {
        let (module, rest) = self.module_operand(expr, args, ModuleOperator::UpdateModule)?;
        let Some((onto, rest)) = rest.split_first() else {
            return self.error(
                expr,
                "`update_module(m, u, { migrate })` takes the live instance and the module to \
                 apply onto it (§13.16)",
            );
        };
        let onto = self.module_argument(onto, ModuleOperator::UpdateModule, arg::ONTO)?;
        let migrate = if rest.is_empty() {
            // §13.16 default: `migrate: model` — migrate the schema and carry the
            // live instance's own data forward.
            TypedExpr::text(expr.span, MigrateAxis::Model.spelling())
        } else {
            let axes = self.axis_object(expr, rest, ModuleOperator::UpdateModule)?;
            self.reject_unknown_axis(&axes, &[arg::MIGRATE], ModuleOperator::UpdateModule)?;
            match axes.get(arg::MIGRATE) {
                None => TypedExpr::text(expr.span, MigrateAxis::Model.spelling()),
                Some(value) => self.migrate_axis(value)?,
            }
        };
        Some(TypedExpr::new(
            expr.span,
            ExprType::scalar(Type::Text),
            TypedKind::Builtin { func: BuiltinFn::UpdateModule, args: vec![module, onto, migrate] },
        ))
    }

    /// `rollback_module(m, @point)` (§13.16) → the selected point identity.
    ///
    /// The coordinate is a `timestamp` (the §13.16 spelling of a retained point's
    /// instant) or a `blob` — the `.liasse` artifact that CARRIES a retained point.
    /// The second form is the only one a CORE instance can always reconstruct, and
    /// it is exactly what `pack` produced at that point; which of the two the
    /// instance can actually honour is the runtime's answer, not the checker's.
    fn check_rollback_module(&mut self, expr: &Expr, args: &[Arg]) -> Option<TypedExpr> {
        let (module, rest) = self.module_operand(expr, args, ModuleOperator::Rollback)?;
        let [Arg::Positional(point)] = rest else {
            return self.error(
                expr,
                "`rollback_module(m, @point)` takes the instance and one retained-point \
                 coordinate — a `timestamp` or the `blob` artifact carrying that point (§13.16)",
            );
        };
        let point = self.check(point)?;
        match point.ty().as_scalar() {
            Some(Type::Timestamp(_) | Type::Blob) => {}
            _ => {
                return self.error(
                    expr,
                    format!(
                        "`rollback_module` addresses a retained point by `timestamp` or by the \
                         `blob` artifact carrying it, but a {} was given (§13.16)",
                        point.ty().describe()
                    ),
                );
            }
        }
        Some(TypedExpr::new(
            expr.span,
            ExprType::scalar(Type::Text),
            TypedKind::Builtin { func: BuiltinFn::RollbackModule, args: vec![module, point] },
        ))
    }

    /// Split an operator's arguments into its leading `module` operand and the rest.
    fn module_operand<'a>(
        &mut self,
        expr: &Expr,
        args: &'a [Arg],
        operator: ModuleOperator,
    ) -> Option<(TypedExpr, &'a [Arg])> {
        let Some((first, rest)) = args.split_first() else {
            self.report(expr, format!("`{}` takes a `module` operand first (§13.16)", operator.name()));
            return None;
        };
        let module = self.module_argument(first, operator, arg::MODULE)?;
        Some((module, rest))
    }

    /// Type one positional argument and require it to be a `module` value. A module
    /// operand BORROWS (§13.16), so no ownership bookkeeping happens here.
    fn module_argument(&mut self, entry: &Arg, operator: ModuleOperator, role: &str) -> Option<TypedExpr> {
        let Arg::Positional(value) = entry else {
            let Arg::Named { value, .. } = entry else { return None };
            self.report(
                value,
                format!(
                    "`{}`'s `{role}` operand is positional: write `{}(m, …)` (§13.16)",
                    operator.name(),
                    operator.name()
                ),
            );
            return None;
        };
        let typed = self.check(value)?;
        if !matches!(typed.ty().as_scalar(), Some(Type::Module(_))) {
            return self.error(
                value,
                format!(
                    "`{}`'s `{role}` operand is a `module` value, but a {} was given (§13.16)",
                    operator.name(),
                    typed.ty().describe()
                ),
            );
        }
        Some(typed)
    }

    /// Parse an operator's trailing axis argument, refusing a positional operand
    /// that is not an object (a third module, a bare literal): an axis the checker
    /// cannot name is an axis the runtime would silently drop.
    fn axis_object<'a>(
        &mut self,
        expr: &Expr,
        args: &'a [Arg],
        operator: ModuleOperator,
    ) -> Option<AxisObject<'a>> {
        let parsed = AxisObject::parse(args);
        if parsed.is_none() {
            self.report(
                expr,
                format!("`{}`'s trailing argument is an axis object `{{ … }}` (§13.16)", operator.name()),
            );
        }
        parsed
    }

    /// Refuse an axis member the operator does not define, by name. Accepting it
    /// silently would report success for a coordinate that never took effect.
    fn reject_unknown_axis(
        &mut self,
        axes: &AxisObject<'_>,
        accepted: &[&str],
        operator: ModuleOperator,
    ) -> Option<()> {
        match axes.unknown(accepted) {
            None => Some(()),
            Some((name, value)) => {
                self.report(
                    value,
                    format!(
                        "`{}` has no `{name}` axis (§13.16); it accepts {} — refusing rather \
                         than silently ignoring it",
                        operator.name(),
                        accepted.join(", ")
                    ),
                );
                None
            }
        }
    }

    /// Type one optional axis operand against its coordinate type, or `none` when
    /// the axis is absent (§13.16: "each axis is optional and defaults to the
    /// current value").
    fn axis_operand(
        &mut self,
        expr: &Expr,
        axes: &AxisObject<'_>,
        name: &str,
        coordinate: AxisCoordinate,
    ) -> Option<TypedExpr> {
        let Some(value) = axes.get(name) else { return Some(TypedExpr::absent(expr.span)) };
        let typed = self.check(value)?;
        if !coordinate.admits(typed.ty().as_scalar()) {
            return self.error(
                value,
                format!(
                    "`pack`'s `{name}` axis is addressed by a {}, but a {} was given (§13.16)",
                    coordinate.describe(),
                    typed.ty().describe()
                ),
            );
        }
        Some(typed)
    }

    /// Type the `migrate` axis: one of the two §13.16 spellings, as a literal, so
    /// an unknown spelling is a load error and not a runtime fallback to `model`.
    fn migrate_axis(&mut self, value: &Expr) -> Option<TypedExpr> {
        let spelling = match &value.kind {
            ExprKind::Str(text) => text.clone(),
            ExprKind::Name(ident) => ident.text.clone(),
            _ => String::new(),
        };
        match MigrateAxis::parse(&spelling) {
            Some(axis) => Some(TypedExpr::text(value.span, axis.spelling())),
            None => self.error(
                value,
                format!(
                    "`update_module`'s `migrate` axis is one of {} (§13.16)",
                    MigrateAxis::SPELLINGS.join(" or ")
                ),
            ),
        }
    }
}
