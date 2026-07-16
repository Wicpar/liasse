//! Driving the expression grammar and lowering pest pairs into the spanned
//! expression AST. Operator layers live here; primary, postfix, selector, and
//! block lowering live in [`super::lower`].

use liasse_diag::{ByteSpan, Diagnostic, Diagnostics, SourceId, Span};
use pest::Parser;
use pest::iterators::Pair;
use pest_derive::Parser;

use crate::clamp;
use crate::error::Report;
use crate::expr::ast::{
    BinaryOp, CombinatorOp, Expr, ExprKind, SpannedExpression, Stmt, StmtKind, UnaryOp,
};
use crate::scan::{check_nesting_depth, Lexis};

#[derive(Parser)]
#[grammar = "expr/grammar.pest"]
pub(crate) struct ExprGrammar;

/// Parses one expression or `$mut` statement into a spanned AST.
///
/// `source` is the [`SourceId`] the caller registered `text` under in a
/// `liasse_diag::SourceMap`; it is used only to locate diagnostics.
pub fn parse_expression(source: SourceId, text: &str) -> Result<SpannedExpression, Diagnostics> {
    // Reject pathologically nested input before pest's recursive descent can
    // overflow the stack (see `scan::check_nesting_depth`).
    check_nesting_depth(source, text, Lexis::Expression)?;
    match ExprGrammar::parse(Rule::program, text) {
        Ok(mut pairs) => {
            let mut builder = Builder {
                source,
                text,
                diags: Diagnostics::new(),
            };
            let Some(statement) = pairs
                .next()
                .and_then(|program| builder.first_inner(&program))
                .and_then(|stmt| builder.statement(stmt))
            else {
                return Err(builder.aborted());
            };
            Ok(SpannedExpression { statement })
        }
        Err(error) => Err(Report::new(source, text, Lexis::Expression).build(error)),
    }
}

pub(crate) struct Builder<'s> {
    source: SourceId,
    text: &'s str,
    diags: Diagnostics,
}

impl Builder<'_> {
    fn statement(&mut self, pair: Pair<'_, Rule>) -> Option<Stmt> {
        let span = self.span(&pair);
        let inner = self.first_inner(&pair)?;
        let kind = match inner.as_rule() {
            Rule::return_stmt => {
                // `return_stmt` = `return_kw ~ expression`; the atomic keyword is
                // present as a leading token, so select the `expression` pair.
                let Some(expr_pair) =
                    inner.into_inner().find(|p| p.as_rule() == Rule::expression)
                else {
                    return self.internal(span);
                };
                StmtKind::Return(self.node(expr_pair)?)
            }
            Rule::tail_stmt => self.tail_stmt(inner)?,
            _ => return self.internal(span),
        };
        Some(Stmt { span, kind })
    }

    fn tail_stmt(&mut self, pair: Pair<'_, Rule>) -> Option<StmtKind> {
        let mut inner = pair.into_inner();
        let expr = self.node(inner.next()?)?;
        match inner.next() {
            None => Some(StmtKind::Bare(expr)),
            Some(tail) => match tail.as_rule() {
                Rule::assign_tail => {
                    let value_pair = self.first_inner(&tail)?;
                    Some(StmtKind::Assign {
                        target: expr,
                        value: self.node(value_pair)?,
                    })
                }
                Rule::clear_tail => Some(StmtKind::Clear(expr)),
                _ => {
                    let span = self.span(&tail);
                    self.internal(span)
                }
            },
        }
    }

    /// Central dispatch: routes an expression-family pair to its handler.
    pub(crate) fn node(&mut self, pair: Pair<'_, Rule>) -> Option<Expr> {
        match pair.as_rule() {
            Rule::expression => {
                let inner = self.first_inner(&pair)?;
                self.node(inner)
            }
            Rule::ternary => self.ternary(pair),
            Rule::combination => self.combination(pair),
            Rule::fallback
            | Rule::logic_or
            | Rule::logic_and
            | Rule::comparison
            | Rule::additive
            | Rule::multiplicative => self.binary_layer(pair),
            Rule::unary => self.unary(pair),
            Rule::postfix => self.postfix(pair),
            Rule::primary => self.primary(pair),
            Rule::block => {
                let span = self.span(&pair);
                Some(Expr {
                    span,
                    kind: ExprKind::Object(self.block_members(pair)?),
                })
            }
            _ => {
                let span = self.span(&pair);
                self.internal(span)
            }
        }
    }

    fn ternary(&mut self, pair: Pair<'_, Rule>) -> Option<Expr> {
        let mut inner = pair.into_inner();
        let cond = self.node(inner.next()?)?;
        match inner.next() {
            None => Some(cond),
            Some(tail) => {
                let mut parts = tail.into_inner();
                let then = self.node(parts.next()?)?;
                let otherwise = self.node(parts.next()?)?;
                let span = cond.span.merge(otherwise.span);
                Some(Expr {
                    span,
                    kind: ExprKind::Ternary {
                        cond: Box::new(cond),
                        then: Box::new(then),
                        otherwise: Box::new(otherwise),
                    },
                })
            }
        }
    }

    fn combination(&mut self, pair: Pair<'_, Rule>) -> Option<Expr> {
        let mut inner = pair.into_inner();
        let first = self.node(inner.next()?)?;
        let mut operands = vec![first];
        let mut operators = Vec::new();
        while let Some(op_pair) = inner.next() {
            operators.push(self.combinator_op(op_pair)?);
            operands.push(self.node(inner.next()?)?);
        }
        if operators.is_empty() {
            return operands.into_iter().next();
        }
        let span = self.span_of_operands(&operands);
        Some(Expr {
            span,
            kind: ExprKind::Combination {
                operands,
                operators,
            },
        })
    }

    fn combinator_op(&mut self, pair: Pair<'_, Rule>) -> Option<CombinatorOp> {
        let inner = self.first_inner(&pair)?;
        match inner.as_rule() {
            Rule::union_op => Some(CombinatorOp::Union),
            Rule::intersect_op => Some(CombinatorOp::Intersect),
            _ => {
                let span = self.span(&inner);
                self.internal(span)
            }
        }
    }

    fn binary_layer(&mut self, pair: Pair<'_, Rule>) -> Option<Expr> {
        let mut inner = pair.into_inner();
        let mut acc = self.node(inner.next()?)?;
        while let Some(op_pair) = inner.next() {
            let op = self.binary_op(op_pair)?;
            let rhs = self.node(inner.next()?)?;
            let span = acc.span.merge(rhs.span);
            acc = Expr {
                span,
                kind: ExprKind::Binary {
                    op,
                    lhs: Box::new(acc),
                    rhs: Box::new(rhs),
                },
            };
        }
        Some(acc)
    }

    fn binary_op(&mut self, pair: Pair<'_, Rule>) -> Option<BinaryOp> {
        match pair.as_rule() {
            Rule::or_op => return Some(BinaryOp::Or),
            Rule::and_op => return Some(BinaryOp::And),
            Rule::fallback_op => return Some(BinaryOp::Fallback),
            _ => {}
        }
        let inner = self.first_inner(&pair)?;
        let op = match inner.as_rule() {
            Rule::eq => BinaryOp::Eq,
            Rule::ne => BinaryOp::Ne,
            Rule::le => BinaryOp::Le,
            Rule::ge => BinaryOp::Ge,
            Rule::lt => BinaryOp::Lt,
            Rule::gt => BinaryOp::Gt,
            Rule::in_op => BinaryOp::In,
            Rule::plus => BinaryOp::Add,
            Rule::minus => BinaryOp::Sub,
            Rule::star => BinaryOp::Mul,
            Rule::slash => BinaryOp::Div,
            Rule::percent => BinaryOp::Rem,
            _ => {
                let span = self.span(&inner);
                return self.internal(span);
            }
        };
        Some(op)
    }

    fn unary(&mut self, pair: Pair<'_, Rule>) -> Option<Expr> {
        let ops: Vec<Pair<'_, Rule>> = pair.into_inner().collect();
        let (operand_pair, prefixes) = ops.split_last()?;
        let mut acc = self.node(operand_pair.clone())?;
        for op_pair in prefixes.iter().rev() {
            let op = self.unary_op(op_pair)?;
            let span = self.span(op_pair).merge(acc.span);
            acc = Expr {
                span,
                kind: ExprKind::Unary {
                    op,
                    operand: Box::new(acc),
                },
            };
        }
        Some(acc)
    }

    fn unary_op(&mut self, pair: &Pair<'_, Rule>) -> Option<UnaryOp> {
        let inner = self.first_inner(pair)?;
        match inner.as_rule() {
            Rule::not_op => Some(UnaryOp::Not),
            Rule::neg_op => Some(UnaryOp::Neg),
            _ => {
                let span = self.span(&inner);
                self.internal(span)
            }
        }
    }

    fn span_of_operands(&self, operands: &[Expr]) -> ByteSpan {
        match (operands.first(), operands.last()) {
            (Some(first), Some(last)) => first.span.merge(last.span),
            _ => ByteSpan::point(0),
        }
    }

    // --- shared plumbing, also used by `super::lower` ---

    pub(crate) fn first_inner<'p>(&mut self, pair: &Pair<'p, Rule>) -> Option<Pair<'p, Rule>> {
        let span = self.span(pair);
        match pair.clone().into_inner().next() {
            Some(inner) => Some(inner),
            None => {
                let _: Option<()> = self.internal(span);
                None
            }
        }
    }

    pub(crate) fn span(&self, pair: &Pair<'_, Rule>) -> ByteSpan {
        let span = pair.as_span();
        ByteSpan::cover(clamp(span.start()), clamp(span.end()))
    }

    pub(crate) fn text(&self) -> &str {
        self.text
    }

    pub(crate) fn internal<T>(&mut self, span: ByteSpan) -> Option<T> {
        self.diags.push(
            Diagnostic::error("internal parser error: unexpected grammar shape")
                .code("syntax-internal")
                .primary(Span::new(self.source, span), "while lowering this node")
                .build(),
        );
        None
    }

    fn aborted(mut self) -> Diagnostics {
        if self.diags.is_empty() {
            let _: Option<()> = self.internal(ByteSpan::point(0));
        }
        self.diags
    }
}
