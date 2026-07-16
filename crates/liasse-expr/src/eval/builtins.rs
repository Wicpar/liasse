//! Evaluation of built-in language and namespace functions (§6.5).
//!
//! `string.trim` uses Rust's Unicode `White_Space` trimming; whether a
//! non-ASCII-whitespace-only string normalizes to empty is unpinned
//! (SPEC-ISSUES item 5), and this crate takes the Unicode-aware reading.

use liasse_value::{Integer, Text, Value};

use crate::env::Cell;
use crate::error::EvalError;
use crate::eval::Evaluator;
use crate::typed::{BuiltinFn, TypedExpr};

impl Evaluator<'_> {
    pub(crate) fn eval_builtin(
        &mut self,
        func: BuiltinFn,
        args: &[TypedExpr],
    ) -> Result<Cell, EvalError> {
        match func {
            BuiltinFn::Size => self.eval_size(args),
            BuiltinFn::Has => self.eval_has(args),
            BuiltinFn::Assert => self.eval_assert(args),
            BuiltinFn::StringLower => self.eval_string(args, str::to_lowercase),
            BuiltinFn::StringUpper => self.eval_string(args, str::to_uppercase),
            BuiltinFn::StringTrim => self.eval_string(args, |text| text.trim().to_owned()),
        }
    }

    fn first(&mut self, args: &[TypedExpr]) -> Result<Cell, EvalError> {
        match args.first() {
            Some(arg) => self.eval(arg),
            None => Err(EvalError::ShapeMismatch { expected: "one argument" }),
        }
    }

    fn eval_size(&mut self, args: &[TypedExpr]) -> Result<Cell, EvalError> {
        let count = match self.first(args)? {
            // §6.5: text length is a Unicode-safe count of scalar values.
            Cell::Scalar(Value::Text(text)) => text.as_str().chars().count(),
            Cell::Scalar(Value::Set(members)) => members.len(),
            Cell::Collection(rows) => rows.len(),
            _ => return Err(EvalError::ShapeMismatch { expected: "text, a set, or a view" }),
        };
        Ok(Cell::Scalar(Value::Int(Integer::from(count as i64))))
    }

    fn eval_has(&mut self, args: &[TypedExpr]) -> Result<Cell, EvalError> {
        let present = match self.first(args)? {
            Cell::Scalar(Value::None) => false,
            Cell::Collection(rows) => !rows.is_empty(),
            _ => true,
        };
        Ok(Cell::Scalar(Value::Bool(present)))
    }

    /// §8.8: an assertion's condition; its admission effect (rejecting a
    /// mutation) is the mutation layer's, so as a value it evaluates to the
    /// condition's truth.
    fn eval_assert(&mut self, args: &[TypedExpr]) -> Result<Cell, EvalError> {
        let verdict = matches!(self.first(args)?, Cell::Scalar(Value::Bool(true)));
        Ok(Cell::Scalar(Value::Bool(verdict)))
    }

    fn eval_string(
        &mut self,
        args: &[TypedExpr],
        transform: impl Fn(&str) -> String,
    ) -> Result<Cell, EvalError> {
        match self.first(args)? {
            Cell::Scalar(Value::Text(text)) => {
                Ok(Cell::Scalar(Value::Text(Text::new(transform(text.as_str())))))
            }
            _ => Err(EvalError::ShapeMismatch { expected: "a text argument" }),
        }
    }
}
