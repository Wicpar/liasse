//! The minimal program environment a face evaluates a residual against.
//!
//! After hoisting (§7.3), a residual reaches only the candidate (`.` and the
//! filter/coverage bind), hoisted synthetic bindings, literals, and pure operators
//! over them. [`ProgEnv`] serves exactly that: `binding` resolves the coverage bind
//! (a `ScopeBinding`) and every hoisted synthetic; every other environment channel
//! is unreachable in an audited residual, so it answers the trait's own "unbound"
//! path rather than fabricating a value. A LOCAL bind (a flat view's filter, a
//! projected output threaded into a sort key) is seeded into the evaluator's frame
//! by `evaluate_bound`, not here.

use liasse_expr::{Cell, Environment, Row, RowId};
use liasse_value::{Precision, Timestamp, Uuid, Value};

/// The candidate + hoisted-env environment for one lowered view's faces.
pub(crate) struct ProgEnv<'a> {
    /// The hoisted candidate-free values, keyed by synthetic name (NUL-prefixed).
    hoisted: &'a [(String, Cell)],
    /// The coverage/filter bind name resolved to the candidate, when the residual
    /// references it as a `ScopeBinding`.
    bind: Option<(&'a str, &'a Cell)>,
    /// An empty root — never read by an audited residual, but the trait needs a
    /// borrowable `Row` to hand back.
    empty_root: Row,
}

impl<'a> ProgEnv<'a> {
    /// The environment for one candidate: the shared hoisted env plus the bind name
    /// resolved to this candidate.
    pub(crate) fn new(hoisted: &'a [(String, Cell)], bind: Option<(&'a str, &'a Cell)>) -> Self {
        Self { hoisted, bind, empty_root: Row::keyless(RowId::leaf(0), std::iter::empty()) }
    }
}

impl Environment for ProgEnv<'_> {
    fn root(&self) -> &Row {
        &self.empty_root
    }

    fn param(&self, _name: &str) -> Option<Cell> {
        None
    }

    fn structural(&self, _name: &str) -> Option<Cell> {
        None
    }

    fn import(&self, _name: &str) -> Option<Cell> {
        None
    }

    fn binding(&self, name: &str) -> Option<Cell> {
        if let Some((bind, candidate)) = self.bind
            && name == bind
        {
            return Some(candidate.clone());
        }
        self.hoisted.iter().find(|(n, _)| n == name).map(|(_, cell)| cell.clone())
    }

    fn now(&self) -> Timestamp {
        // Unreachable in an audited residual (`now()` is external and hoisted); a
        // fixed sample keeps the method total without fabricating live state.
        Timestamp::new(0, Precision::DEFAULT)
    }

    fn uuid(&self, _site: liasse_expr::CallSite) -> Uuid {
        // Unreachable in an audited residual (`uuid()` is external and hoisted).
        Uuid::from_bytes([0; 16])
    }
}

/// Whether a value is the strict-truthy `Bool(true)` a predicate face consumes
/// (§7.2): anything else — including `none` — reads as `false`.
#[must_use]
pub(crate) fn is_true(cell: &Cell) -> bool {
    matches!(cell.as_scalar(), Some(Value::Bool(true)))
}
