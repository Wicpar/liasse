//! §8.3 parameter inference within structured §16.4 host-call arguments.
//!
//! A host argument is a typed position supplied by the resolved namespace
//! descriptor. Object fields and list elements refine that position as the
//! syntax walk descends; any position the descriptor cannot name stays
//! deliberately permissive as `json`. This pass only fills parameters the
//! ordinary prototype/state inference did not already pin.

use liasse_expr::ExprType;
use liasse_syntax::{BlockMember, BlockMemberKind, Expr, ExprKind};
use liasse_value::Type;

use crate::host::HostDescriptors;
use crate::walk::child_exprs;

use super::helpers::{arg_expr, host_call_target, record, Params};

/// The descriptor position occupied by one expression within a host argument.
#[derive(Clone, Copy)]
enum HostArgPosition<'a> {
    /// The resolved host descriptor pins this position to a concrete type.
    Declared(&'a Type),
    /// No descriptor type reaches this position; bind it at the permissive top.
    Unpinned,
}

impl<'a> HostArgPosition<'a> {
    /// The contract type of a parameter directly occupying this position.
    fn param_type(self) -> ExprType {
        match self {
            Self::Declared(ty) => ExprType::scalar(ty.clone()),
            Self::Unpinned => ExprType::scalar(Type::Json),
        }
    }

    /// Refine an object literal to one descriptor struct field.
    fn field(self, name: &str) -> Self {
        match self.present_type() {
            Some(Type::Struct(fields)) => fields.field(name).map_or(Self::Unpinned, Self::Declared),
            _ => Self::Unpinned,
        }
    }

    /// Refine a list literal to its descriptor set element.
    fn element(self) -> Self {
        match self.present_type() {
            Some(Type::Set(element)) => Self::Declared(element),
            _ => Self::Unpinned,
        }
    }

    /// Peel optional containers when a present object/list literal occupies the
    /// position. A bare parameter retains the optional type through
    /// [`Self::param_type`].
    fn present_type(self) -> Option<&'a Type> {
        let Self::Declared(mut ty) = self else {
            return None;
        };
        while let Type::Optional(inner) = ty {
            ty = inner;
        }
        Some(ty)
    }
}

/// The gap-filling host-argument inference pass over one mutation program.
pub(super) struct HostArgInference<'a> {
    hosts: &'a HostDescriptors,
}

impl<'a> HostArgInference<'a> {
    pub(super) const fn new(hosts: &'a HostDescriptors) -> Self {
        Self { hosts }
    }

    /// Find every host call below `expr`, processing nested calls before their
    /// enclosing argument so the nested call's own signature is the strongest
    /// available descriptor for its parameters.
    pub(super) fn infer(&self, expr: &Expr, params: &mut Params) {
        for child in child_exprs(expr) {
            self.infer(child, params);
        }
        let ExprKind::Call { callee, args } = &expr.kind else {
            return;
        };
        let Some((namespace, function)) = host_call_target(callee) else {
            return;
        };
        let signature = self.hosts.op(namespace, function);
        for (index, arg) in args.iter().enumerate() {
            let position = signature
                .and_then(|op| op.params().get(index))
                .map_or(HostArgPosition::Unpinned, HostArgPosition::Declared);
            self.infer_argument(arg_expr(arg), position, params);
        }
    }

    /// Descend through arbitrary object/list combinations, refining the
    /// descriptor type at every structural step.
    fn infer_argument(&self, expr: &Expr, position: HostArgPosition<'_>, params: &mut Params) {
        match &expr.kind {
            ExprKind::Param(id) => {
                if !params.contains(&id.text) {
                    record(params, &id.text, position.param_type());
                }
            }
            ExprKind::Object(members) => {
                for member in members {
                    self.infer_member(member, position, params);
                }
            }
            ExprKind::List(items) => {
                let element = position.element();
                for item in items {
                    self.infer_argument(item, element, params);
                }
            }
            // A nested call was already visited by [`Self::infer`] with its own
            // signature. Do not smear the enclosing structural position into
            // that call's internal arguments.
            ExprKind::Call { .. } => {}
            _ => {
                for child in child_exprs(expr) {
                    self.infer_argument(child, HostArgPosition::Unpinned, params);
                }
            }
        }
    }

    /// Descend through one object member, preserving its literal field name
    /// when the syntax carries one.
    fn infer_member(
        &self,
        member: &BlockMember,
        position: HostArgPosition<'_>,
        params: &mut Params,
    ) {
        match &member.kind {
            BlockMemberKind::Named {
                name,
                value: Some(value),
            } => self.infer_argument(value, position.field(&name.text), params),
            BlockMemberKind::Shorthand(value) => {
                let field = match &value.kind {
                    ExprKind::Param(name) | ExprKind::Name(name) => position.field(&name.text),
                    _ => HostArgPosition::Unpinned,
                };
                self.infer_argument(value, field, params);
            }
            // These forms are not valid object-literal members, but keeping the
            // traversal total lets the ordinary expression diagnostic remain
            // the authority while parameters still bind honestly.
            BlockMemberKind::Directive { value, .. } | BlockMemberKind::Assign { value, .. } => {
                self.infer_argument(value, HostArgPosition::Unpinned, params);
            }
            BlockMemberKind::Named { value: None, .. } | BlockMemberKind::Clear(_) => {}
        }
    }
}
