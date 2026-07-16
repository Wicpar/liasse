//! The typed request an external caller submits (§8.5, §10): a named mutation,
//! the key of the selected receiver row (for a row mutation), and typed
//! arguments. Callers provide already-typed [`Value`]s — parse, don't validate.

use std::collections::BTreeMap;

use liasse_value::Value;

/// A mutation call: which operation, which receiver row, and what arguments.
#[derive(Debug, Clone)]
pub struct CallRequest {
    mutation: String,
    receiver: Vec<Value>,
    args: BTreeMap<String, Value>,
}

impl CallRequest {
    /// A call of the mutation named `mutation`.
    #[must_use]
    pub fn new(mutation: impl Into<String>) -> Self {
        Self { mutation: mutation.into(), receiver: Vec::new(), args: BTreeMap::new() }
    }

    /// Append a receiver key component (§8.2). A single-field key needs one; a
    /// composite key needs each component in `$key` order.
    #[must_use]
    pub fn receiver(mut self, component: Value) -> Self {
        self.receiver.push(component);
        self
    }

    /// Bind a mutation argument `@name` to a typed value (§8.3).
    #[must_use]
    pub fn arg(mut self, name: impl Into<String>, value: Value) -> Self {
        self.args.insert(name.into(), value);
        self
    }

    /// The mutation name.
    #[must_use]
    pub fn mutation(&self) -> &str {
        &self.mutation
    }

    /// The receiver key components, in `$key` order.
    #[must_use]
    pub fn receiver_key(&self) -> &[Value] {
        &self.receiver
    }

    /// The argument bound to `name`, if supplied.
    #[must_use]
    pub fn arg_value(&self, name: &str) -> Option<&Value> {
        self.args.get(name)
    }
}
