//! How an exposed surface member maps onto a runtime view or mutation (§10.1,
//! §10.4).
//!
//! A surface grants *named* access to package-defined expressions: the wire
//! carries the surface name, the call name, and typed values, never an
//! executable expression (§10.1). These bindings are that mapping — a surface
//! `$view` to a runtime view, and each surface `$mut` external name to a runtime
//! mutation plus the argument roles (§10.1: "the surface parameters are the
//! selector parameters combined with the referenced mutation's parameters").
//!
//! The model validates that every `$mut` reference names a declared mutation but
//! retains neither the reference nor the receiver split
//! (`crates/liasse-model/src/surface.rs`); this layer carries it explicitly and
//! re-validates it against the model's exposed surfaces when a router is built.

use std::collections::BTreeMap;

/// One exposed surface's bindings: its optional `$view` and its `$mut` calls
/// keyed by external call name (§10.1). Built by the host and re-validated
/// against the model's exposed surfaces when a router is assembled.
#[derive(Debug, Clone)]
pub struct SurfaceBinding {
    view: Option<ViewBinding>,
    calls: BTreeMap<String, CallBinding>,
}

impl SurfaceBinding {
    /// An empty surface exposing neither a view nor any call. Members are added
    /// with [`SurfaceBinding::with_view`] and [`SurfaceBinding::with_call`].
    #[must_use]
    pub fn new() -> Self {
        Self { view: None, calls: BTreeMap::new() }
    }

    /// Set this surface's `$view` binding.
    #[must_use]
    pub fn with_view(mut self, binding: ViewBinding) -> Self {
        self.view = Some(binding);
        self
    }

    /// Add a `$mut` call under external `name`.
    #[must_use]
    pub fn with_call(mut self, name: impl Into<String>, binding: CallBinding) -> Self {
        self.calls.insert(name.into(), binding);
        self
    }

    /// The `$view` binding, if the surface exposes one.
    #[must_use]
    pub fn view(&self) -> Option<&ViewBinding> {
        self.view.as_ref()
    }

    /// The `$mut` call bound to external `name`, if any.
    #[must_use]
    pub fn call(&self, name: &str) -> Option<&CallBinding> {
        self.calls.get(name)
    }

    /// The external call names this surface exposes.
    pub fn call_names(&self) -> impl Iterator<Item = &String> {
        self.calls.keys()
    }
}

impl Default for SurfaceBinding {
    fn default() -> Self {
        Self::new()
    }
}

/// A surface `$view` binding: the runtime view it reads (§10.1).
#[derive(Debug, Clone)]
pub struct ViewBinding {
    view: String,
}

impl ViewBinding {
    /// A view binding onto the runtime view named `view`.
    #[must_use]
    pub fn new(view: impl Into<String>) -> Self {
        Self { view: view.into() }
    }

    /// The runtime view name.
    #[must_use]
    pub fn view(&self) -> &str {
        &self.view
    }
}

/// A surface `$mut` binding: the runtime mutation an external call name invokes,
/// and how the call's arguments split into the mutation's receiver key and its
/// parameters (§10.1).
///
/// A row mutation selects exactly one receiver before naming the mutation
/// (§10.1); `receiver` lists the argument names forming that receiver key in
/// `$key` order, and `params` lists the argument names bound as mutation
/// parameters. A root mutation has an empty `receiver`.
#[derive(Debug, Clone)]
pub struct CallBinding {
    mutation: String,
    receiver: Vec<String>,
    params: Vec<String>,
}

impl CallBinding {
    /// A binding onto the runtime mutation named `mutation`, taking no receiver
    /// key (a root or struct mutation) and the listed argument names as
    /// parameters.
    #[must_use]
    pub fn root(mutation: impl Into<String>, params: impl IntoIterator<Item = String>) -> Self {
        Self { mutation: mutation.into(), receiver: Vec::new(), params: params.into_iter().collect() }
    }

    /// A binding onto a row mutation, taking `receiver` argument names as the
    /// selected row's key (in `$key` order) and `params` as its parameters.
    #[must_use]
    pub fn row(
        mutation: impl Into<String>,
        receiver: impl IntoIterator<Item = String>,
        params: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            mutation: mutation.into(),
            receiver: receiver.into_iter().collect(),
            params: params.into_iter().collect(),
        }
    }

    /// The runtime mutation name.
    #[must_use]
    pub fn mutation(&self) -> &str {
        &self.mutation
    }

    /// The argument names forming the receiver key, in `$key` order.
    #[must_use]
    pub fn receiver(&self) -> &[String] {
        &self.receiver
    }

    /// The argument names bound as mutation parameters.
    #[must_use]
    pub fn params(&self) -> &[String] {
        &self.params
    }
}
