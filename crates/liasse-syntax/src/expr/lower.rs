//! Lowering of primary expressions, postfix chains, selectors, call
//! arguments, and projection/patch blocks. The operator layers live in
//! [`super::parse`]; this file continues the same [`Builder`] impl.

use liasse_diag::ByteSpan;
use pest::iterators::Pair;

use crate::error::RuleLabel;
use crate::expr::ast::{
    Arg, BlockMember, BlockMemberKind, Expr, ExprKind, Ident, Selector,
};
use crate::expr::parse::{Builder, Rule};
use crate::text::RawString;

impl Builder<'_> {
    pub(super) fn primary(&mut self, pair: Pair<'_, Rule>) -> Option<Expr> {
        let span = self.span(&pair);
        let inner = self.first_inner(&pair)?;
        let kind = match inner.as_rule() {
            Rule::none_kw => ExprKind::None,
            Rule::boolean => ExprKind::Bool(inner.as_str() == "true"),
            Rule::decimal => ExprKind::Decimal(inner.as_str().to_owned()),
            Rule::integer => ExprKind::Int(inner.as_str().to_owned()),
            Rule::string => ExprKind::Str(RawString::Quoted(inner.as_str()).decode()),
            Rule::multiline => ExprKind::Str(self.multiline(&inner)),
            Rule::list => ExprKind::List(self.expr_list(inner)?),
            Rule::object_literal => ExprKind::Object(self.block_members(inner)?),
            Rule::root_ref => return self.root_ref(inner, span),
            Rule::current_ref => ExprKind::Current,
            Rule::implicit_field => return self.implicit_field(inner),
            Rule::parent_ref => ExprKind::Parent(inner.as_str().chars().count() as u32),
            Rule::import_ref => ExprKind::Import(self.ident_of(inner)?),
            Rule::param_ref => ExprKind::Param(self.ident_of(inner)?),
            Rule::struct_ref => ExprKind::Structural(self.ident_of(inner)?),
            Rule::name_ref => ExprKind::Name(self.ident_of(inner)?),
            Rule::grouped => {
                let expr_pair = self.first_inner(&inner)?;
                return self.node(expr_pair);
            }
            _ => return self.internal(span),
        };
        Some(Expr { span, kind })
    }

    /// `/` (root) or `/name` (a member of the root).
    fn root_ref(&mut self, pair: Pair<'_, Rule>, span: ByteSpan) -> Option<Expr> {
        let root = Expr {
            span: ByteSpan::at(span.start(), 1),
            kind: ExprKind::Root,
        };
        match pair.into_inner().next() {
            Some(member_pair) => {
                let member = self.ident_of(member_pair)?;
                Some(Expr {
                    span,
                    kind: ExprKind::Field {
                        base: Box::new(root),
                        member,
                    },
                })
            }
            None => Some(Expr {
                span,
                kind: ExprKind::Root,
            }),
        }
    }

    fn implicit_field(&mut self, pair: Pair<'_, Rule>) -> Option<Expr> {
        // `implicit_field = "." ~ member_ref` — a leading `.field` reads the
        // field from the current value; the base is a `.` spanning the dot.
        let span = self.span(&pair);
        let dot = ByteSpan::at(span.start(), 1);
        let member_pair = self.first_inner(&pair)?;
        let member = self.ident_of(member_pair)?;
        Some(Expr {
            span,
            kind: ExprKind::Field {
                base: Box::new(Expr::current(dot)),
                member,
            },
        })
    }

    pub(super) fn postfix(&mut self, pair: Pair<'_, Rule>) -> Option<Expr> {
        let mut inner = pair.into_inner();
        let mut acc = self.primary(inner.next()?)?;
        for op in inner {
            acc = self.apply_postfix(acc, op)?;
        }
        Some(acc)
    }

    fn apply_postfix(&mut self, base: Expr, op: Pair<'_, Rule>) -> Option<Expr> {
        let op_span = self.span(&op);
        let span = base.span.merge(op_span);
        let inner = self.first_inner(&op)?;
        let kind = match inner.as_rule() {
            Rule::field_access => {
                let member_pair = self.first_inner(&inner)?;
                ExprKind::Field {
                    base: Box::new(base),
                    member: self.ident_of(member_pair)?,
                }
            }
            Rule::same_name => {
                let member_pair = self.first_inner(&inner)?;
                ExprKind::SameName {
                    base: Box::new(base),
                    member: self.ident_of(member_pair)?,
                }
            }
            Rule::selector => ExprKind::Select {
                base: Box::new(base),
                selector: self.selector(inner)?,
            },
            Rule::call_args => ExprKind::Call {
                callee: Box::new(base),
                args: self.call_args(inner)?,
            },
            Rule::block => ExprKind::Block {
                base: Box::new(base),
                members: self.block_members(inner)?,
            },
            _ => return self.internal(span),
        };
        Some(Expr { span, kind })
    }

    fn selector(&mut self, pair: Pair<'_, Rule>) -> Option<Selector> {
        // `selector = "[" ~ selector_body ~ "]"`, and
        // `selector_body = bind_selector | key_list` — unwrap both layers.
        let wrapper = self.first_inner(&pair)?;
        let body = self.first_inner(&wrapper)?;
        match body.as_rule() {
            Rule::bind_selector => {
                let mut inner = body.into_inner();
                let name = self.ident_of(inner.next()?)?;
                let condition = match inner.next() {
                    Some(filter) => {
                        let expr_pair = self.first_inner(&filter)?;
                        Some(Box::new(self.node(expr_pair)?))
                    }
                    None => None,
                };
                Some(Selector::Bind { name, condition })
            }
            Rule::key_list => {
                let mut keys = Vec::new();
                for key in body.into_inner() {
                    keys.push(self.node(key)?);
                }
                Some(Selector::Keys(keys))
            }
            _ => {
                let span = self.span(&body);
                self.internal(span)
            }
        }
    }

    fn call_args(&mut self, pair: Pair<'_, Rule>) -> Option<Vec<Arg>> {
        let mut args = Vec::new();
        for arg in pair.into_inner() {
            let inner = self.first_inner(&arg)?;
            let value = match inner.as_rule() {
                Rule::named_arg => {
                    let mut parts = inner.into_inner();
                    let name = self.ident_of(parts.next()?)?;
                    let value_pair = parts.next()?;
                    Arg::Named {
                        name,
                        value: self.node(value_pair)?,
                    }
                }
                _ => Arg::Positional(self.node(inner)?),
            };
            args.push(value);
        }
        Some(args)
    }

    pub(super) fn block_members(&mut self, pair: Pair<'_, Rule>) -> Option<Vec<BlockMember>> {
        let mut members = Vec::new();
        for member in pair.into_inner() {
            if member.as_rule() == Rule::block_member {
                members.push(self.block_member(member)?);
            }
        }
        Some(members)
    }

    fn block_member(&mut self, pair: Pair<'_, Rule>) -> Option<BlockMember> {
        let span = self.span(&pair);
        let inner = self.first_inner(&pair)?;
        let kind = match inner.as_rule() {
            Rule::directive => {
                let mut parts = inner.into_inner();
                let name = self.ident_of(parts.next()?)?;
                let value_pair = parts.next()?;
                BlockMemberKind::Directive {
                    name,
                    value: self.node(value_pair)?,
                }
            }
            Rule::clear_member => {
                let member_pair = self.first_inner(&inner)?;
                BlockMemberKind::Clear(self.ident_of(member_pair)?)
            }
            Rule::named_member => {
                let mut parts = inner.into_inner();
                let name = self.ident_of(parts.next()?)?;
                let value = match parts.next() {
                    Some(value_pair) => Some(self.node(value_pair)?),
                    None => None,
                };
                BlockMemberKind::Named { name, value }
            }
            Rule::assign_member => {
                let mut parts = inner.into_inner();
                let target = self.ident_of(parts.next()?)?;
                let value_pair = parts.next()?;
                BlockMemberKind::Assign {
                    target,
                    value: self.node(value_pair)?,
                }
            }
            Rule::shorthand_member => {
                let expr_pair = self.first_inner(&inner)?;
                BlockMemberKind::Shorthand(self.node(expr_pair)?)
            }
            _ => return self.internal(span),
        };
        Some(BlockMember { span, kind })
    }

    fn expr_list(&mut self, pair: Pair<'_, Rule>) -> Option<Vec<Expr>> {
        let mut items = Vec::new();
        for item in pair.into_inner() {
            items.push(self.node(item)?);
        }
        Some(items)
    }

    /// Builds an [`Ident`] from a sigil-bearing reference (`#name`, `@name`,
    /// `$name`, `member_ref`) or a bare `ident` pair.
    fn ident_of(&mut self, pair: Pair<'_, Rule>) -> Option<Ident> {
        let span = self.span(&pair);
        let structural = pair.as_str().starts_with('$');
        let text = if pair.as_rule() == Rule::ident {
            pair.as_str().to_owned()
        } else {
            self.first_inner(&pair)?.as_str().to_owned()
        };
        Some(Ident {
            span,
            text,
            structural,
        })
    }

    fn multiline(&self, pair: &Pair<'_, Rule>) -> String {
        let start = pair.as_span().start();
        let prefix = self.text().get(..start).unwrap_or("");
        let line_start = prefix.rfind('\n').map_or(0, |n| n + 1);
        let gutter = self
            .text()
            .get(line_start..start)
            .map_or(0, |s| s.chars().count());
        let body = pair
            .clone()
            .into_inner()
            .next()
            .map_or("", |body| body.as_str());
        RawString::Multiline { body, gutter }.decode()
    }
}

impl RuleLabel for Rule {
    fn label(self) -> Option<&'static str> {
        Some(match self {
            Rule::program | Rule::statement => "a statement or expression",
            Rule::expression | Rule::ternary => "an expression",
            Rule::return_stmt => "`return`",
            Rule::primary => "a value, path, or selector",
            Rule::selector => "a `[ ... ]` selector",
            Rule::selector_body => "a key or `:binding`",
            Rule::block | Rule::object_literal => "a `{ ... }` block",
            Rule::call_args => "a `( ... )` argument list",
            Rule::ident | Rule::member_ref => "a name",
            Rule::string => "a quoted string",
            Rule::multiline => "a `'''` multiline string",
            Rule::integer | Rule::decimal => "a number",
            Rule::EOI => "end of input",
            _ => return None,
        })
    }
}
