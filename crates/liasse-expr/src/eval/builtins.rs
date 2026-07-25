//! Evaluation of built-in language and namespace functions (§6.5).
//!
//! `string.trim` uses Rust's Unicode `White_Space` trimming; whether a
//! non-ASCII-whitespace-only string normalizes to empty is unpinned
//! (SPEC-ISSUES item 5), and this crate takes the Unicode-aware reading.
//! `string.lower`/`string.upper` are Rust's Unicode default case mappings;
//! `string.casefold` is the Unicode *default full* case fold (`caseless`,
//! CaseFolding.txt C+F), which §6.5 names and B.1 uses for its case-insensitive
//! `$sort` key — a different operation from lowercasing.

use liasse_value::{Integer, ModuleHandle, Text, Value};

use crate::env::Cell;
use crate::error::EvalError;
use crate::eval::Evaluator;
use crate::lifecycle::ModuleOperator;
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
            // §6.5: `string.casefold` applies the Unicode *default* case fold —
            // the full (C+F) mapping of CaseFolding.txt, distinct from default
            // lowercasing (ß → "ss", final ς → σ, Kelvin K → k). It supplies the
            // B.1 canonical case-insensitive sort key `string.casefold(name)`.
            BuiltinFn::StringCasefold => {
                self.eval_string(args, caseless::default_case_fold_str)
            }
            BuiltinFn::StringTrim => self.eval_string(args, |text| text.trim().to_owned()),
            BuiltinFn::TimeDuration => self.eval_time_duration(args),
            BuiltinFn::Unpack => self.eval_unpack(args),
            // §13.16/§13.10: the lifecycle operators are host-privileged transition
            // effects over engines this evaluator cannot reach. The runtime
            // intercepts them before evaluation, so reaching here is a contract
            // breach — refused loudly, never served a fabricated blob, identity,
            // or point.
            BuiltinFn::Pack => Self::refuse_lifecycle(ModuleOperator::Pack),
            BuiltinFn::UpdateModule => Self::refuse_lifecycle(ModuleOperator::UpdateModule),
            BuiltinFn::RollbackModule => Self::refuse_lifecycle(ModuleOperator::Rollback),
            BuiltinFn::ReinstallModule => Self::refuse_lifecycle(ModuleOperator::Reinstall),
        }
    }

    /// The refusal a host-privileged §13.16 operator gets in a pure position.
    fn refuse_lifecycle(operator: ModuleOperator) -> Result<Cell, EvalError> {
        Err(EvalError::ModuleLifecycle { operator: operator.name() })
    }

    /// `unpack(blob)` (§13.16): read a blob into a move-only `module` value with
    /// materialization DEFERRED — the value carries the source blob descriptor as a
    /// `Pending` handle, and no decode/mount happens here. Decode, mount, and state
    /// reconstruction occur only when the value is applied or read (a later piece).
    fn eval_unpack(&mut self, args: &[TypedExpr]) -> Result<Cell, EvalError> {
        match self.first(args)? {
            Cell::Scalar(Value::Blob(descriptor)) => {
                Ok(Cell::Scalar(Value::Module(ModuleHandle::Pending(descriptor))))
            }
            _ => Err(EvalError::ShapeMismatch { expected: "a blob argument" }),
        }
    }

    /// `time.duration(text)` (§16.1): parse an ISO-8601 duration literal (`P30D`) to
    /// a `duration` value. A non-text argument or an unparseable literal is a shape
    /// mismatch — the same class the other core builtins report for bad input.
    fn eval_time_duration(&mut self, args: &[TypedExpr]) -> Result<Cell, EvalError> {
        let text = match self.first(args)? {
            Cell::Scalar(Value::Text(text)) => text.as_str().to_owned(),
            Cell::Scalar(Value::Json(liasse_value::Json::String(text))) => text,
            _ => return Err(EvalError::ShapeMismatch { expected: "a text argument" }),
        };
        liasse_value::Duration::parse(&text)
            .map(|duration| Cell::Scalar(Value::Duration(duration)))
            .map_err(|_| EvalError::ShapeMismatch { expected: "a valid ISO-8601 duration" })
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

    /// A resolved host-namespace call (§16.2/§16.3): evaluate each argument to a
    /// scalar value and defer the call to the environment's host-call hook, which
    /// performs it through the bound host component. The checker has already
    /// checked the arguments against the pinned signature, so a non-scalar
    /// argument here is an environment/type contract breach.
    pub(crate) fn eval_host_call(
        &mut self,
        namespace: &str,
        function: &str,
        args: &[TypedExpr],
    ) -> Result<Cell, EvalError> {
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            match self.eval(arg)? {
                Cell::Scalar(value) => values.push(value),
                // §6.3/§5.6: a single row where a scalar is required is its key.
                Cell::Row(row) => values.push(row.key().clone()),
                Cell::Collection(_) => {
                    return Err(EvalError::ShapeMismatch { expected: "a scalar host-call argument" });
                }
            }
        }
        self.env
            .host_call(namespace, function, &values)
            .map(Cell::Scalar)
    }
}
