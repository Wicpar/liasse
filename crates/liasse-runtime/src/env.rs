//! The deterministic evaluation [`Environment`] an admission or view read runs
//! against (§8.12).
//!
//! It owns the materialized package-root [`Row`], the request's parameter cells,
//! and the two generative samples fixed once per request: `now()` (A.5) and the
//! seed behind `uuid()` (SPEC-ISSUES item 4, per-call-site). Every method is a
//! pure lookup, so "same environment ⇒ same result" holds by construction.

use std::collections::BTreeMap;

use liasse_expr::{CallSite, Cell, Environment, Row};
use liasse_value::{Timestamp, Uuid};

use crate::generator::derive_uuid;

/// A read-only, deterministic evaluation context.
pub(crate) struct RuntimeEnv {
    root: Row,
    params: BTreeMap<String, Cell>,
    now: Timestamp,
    seed: u64,
}

impl RuntimeEnv {
    /// Build the context from a materialized `root`, the request `params`, and
    /// the fixed generative samples.
    pub(crate) fn new(root: Row, params: BTreeMap<String, Cell>, now: Timestamp, seed: u64) -> Self {
        Self { root, params, now, seed }
    }
}

impl Environment for RuntimeEnv {
    fn root(&self) -> &Row {
        &self.root
    }

    fn param(&self, name: &str) -> Option<Cell> {
        self.params.get(name).cloned()
    }

    fn structural(&self, _name: &str) -> Option<Cell> {
        None
    }

    fn import(&self, _name: &str) -> Option<Cell> {
        None
    }

    fn now(&self) -> Timestamp {
        self.now
    }

    fn uuid(&self, site: CallSite) -> Uuid {
        derive_uuid(self.seed, site.span())
    }
}
