//! Typed external requests a client submits to a [`SurfaceHost`] (§12.1).
//!
//! Clients send names and typed values, never executable source (§10.1). These
//! types carry exactly that: a dotted address, typed argument values, the
//! connection, and the §11.4 authenticator selection / §12.3 operation
//! identifier when attached.
//!
//! [`SurfaceHost`]: crate::SurfaceHost

use std::collections::BTreeMap;

use liasse_value::Value;

use crate::address::SurfaceAddress;
use crate::authn::Credential;

/// A per-request authenticator selection (§11.4): the named authenticator and the
/// credential to verify. Overrides the connection's stored context for this one
/// request.
#[derive(Debug, Clone)]
pub struct AuthSelection {
    auth: String,
    credential: Credential,
}

impl AuthSelection {
    /// Select authenticator `auth`, verifying `credential`.
    #[must_use]
    pub fn new(auth: impl Into<String>, credential: Credential) -> Self {
        Self { auth: auth.into(), credential }
    }

    /// The selected authenticator name.
    #[must_use]
    pub fn auth(&self) -> &str {
        &self.auth
    }

    /// The credential to verify.
    #[must_use]
    pub fn credential(&self) -> &Credential {
        &self.credential
    }
}

/// A request to authenticate a context on a connection (§11.4, §11.8): the role
/// whose authenticators are accepted, the selection, and the local context name.
#[derive(Debug, Clone)]
pub struct Authenticate {
    role: String,
    selection: AuthSelection,
    context: String,
}

impl Authenticate {
    /// Authenticate the default context against `role` with `selection`.
    #[must_use]
    pub fn new(role: impl Into<String>, selection: AuthSelection) -> Self {
        Self {
            role: role.into(),
            selection,
            context: crate::connection::DEFAULT_CONTEXT.to_owned(),
        }
    }

    /// Name the context created by this authentication (§11.8 multiplexing).
    #[must_use]
    pub fn as_context(mut self, context: impl Into<String>) -> Self {
        self.context = context.into();
        self
    }

    /// The role whose accepted authenticators gate the selection.
    #[must_use]
    pub fn role(&self) -> &str {
        &self.role
    }

    /// The authenticator selection.
    #[must_use]
    pub fn selection(&self) -> &AuthSelection {
        &self.selection
    }

    /// The local context name to bind.
    #[must_use]
    pub fn context(&self) -> &str {
        &self.context
    }
}

/// A mutation call over a surface (§12.1 `call`).
#[derive(Debug, Clone)]
pub struct SurfaceCall {
    address: SurfaceAddress,
    args: BTreeMap<String, Value>,
    operation_id: Option<String>,
    auth: Option<AuthSelection>,
    context: Option<String>,
}

impl SurfaceCall {
    /// A call to `address` with `args`, no operation id, using the connection's
    /// default authentication context.
    #[must_use]
    pub fn new(address: SurfaceAddress, args: BTreeMap<String, Value>) -> Self {
        Self { address, args, operation_id: None, auth: None, context: None }
    }

    /// Attach a §12.3 operation identifier.
    #[must_use]
    pub fn with_operation_id(mut self, id: impl Into<String>) -> Self {
        self.operation_id = Some(id.into());
        self
    }

    /// Attach a per-request authenticator selection (§11.4).
    #[must_use]
    pub fn with_auth(mut self, auth: AuthSelection) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Select a named authentication context on a multiplexed connection (§11.8).
    #[must_use]
    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }

    /// The targeted surface address.
    #[must_use]
    pub fn address(&self) -> &SurfaceAddress {
        &self.address
    }

    /// The supplied arguments.
    #[must_use]
    pub fn args(&self) -> &BTreeMap<String, Value> {
        &self.args
    }

    /// The attached operation identifier, if any.
    #[must_use]
    pub fn operation_id(&self) -> Option<&str> {
        self.operation_id.as_deref()
    }

    /// The per-request authenticator selection, if any.
    #[must_use]
    pub fn auth(&self) -> Option<&AuthSelection> {
        self.auth.as_ref()
    }

    /// The selected context name, if any.
    #[must_use]
    pub fn context(&self) -> Option<&str> {
        self.context.as_deref()
    }
}

/// A request to open a live subscription over a surface view (§12.1 `view`).
#[derive(Debug, Clone)]
pub struct SurfaceWatch {
    address: SurfaceAddress,
    id: String,
    context: Option<String>,
}

impl SurfaceWatch {
    /// A subscription named `id` over `address`, using the connection's default
    /// context.
    #[must_use]
    pub fn new(address: SurfaceAddress, id: impl Into<String>) -> Self {
        Self { address, id: id.into(), context: None }
    }

    /// Select a named authentication context (§11.8).
    #[must_use]
    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }

    /// The targeted surface address.
    #[must_use]
    pub fn address(&self) -> &SurfaceAddress {
        &self.address
    }

    /// The subscription id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The selected context name, if any.
    #[must_use]
    pub fn context(&self) -> Option<&str> {
        self.context.as_deref()
    }
}
