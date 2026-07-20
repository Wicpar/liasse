//! The decode contract a mounted app carries (§10.1, §11.3, §12.1).
//!
//! A client sends names and *untyped* wire values; the server turns each into a
//! canonical [`Value`](liasse_value::Value) by decoding it against the type the
//! model declares for it — parse, don't validate (AGENTS.md). This module holds
//! that contract: per call address, the typed argument set (receiver keys plus
//! parameters); per view address, the typed `$params`; per authenticator, the
//! `$credential` type. It is deliberately decoupled from `liasse-model`: the caller
//! that wires a [`SurfaceRouter`](liasse_surface::SurfaceRouter) already knows the
//! address→mutation mapping the model does not expose, so it supplies the typed
//! contracts (its own bindings crossed with the model's inferred parameter types),
//! and this crate never re-derives the exposure boundary.
//!
//! The [`Schema`] holds only *shapes to decode into*; authority, membership, and
//! admission stay server-side in the host. A name absent from the schema decodes to
//! the empty argument set, so an over-eager client cannot smuggle an unmodeled
//! argument through — an unknown member is rejected at decode.

use std::collections::BTreeMap;

use liasse_value::Type;

/// The typed decode contract of one mounted app.
#[derive(Debug, Clone, Default)]
pub struct Schema {
    calls: BTreeMap<String, Vec<(String, Type)>>,
    views: BTreeMap<String, Vec<(String, Type)>>,
    credentials: BTreeMap<String, Type>,
    /// Each native-cose `$verify: "cose.verify(/ring, $credential)"` authenticator
    /// name mapped to the keyring it verifies against (§17.7). The connector gates
    /// such a credential through the engine's cose verify before it reaches the
    /// surface authenticator, so — unlike `credentials` — a cose credential is not
    /// a scalar the decoder shapes but a token verified against `ring`.
    cose_rings: BTreeMap<String, String>,
}

impl Schema {
    /// Start building a schema.
    #[must_use]
    pub fn builder() -> SchemaBuilder {
        SchemaBuilder::default()
    }

    /// The typed argument contract of the call at `address` — receiver keys and
    /// parameters, each with its declared type. Empty for an unregistered address
    /// (a client argument for it is then an unknown member, rejected at decode).
    #[must_use]
    pub fn call_args(&self, address: &str) -> &[(String, Type)] {
        self.calls.get(address).map_or(&[], Vec::as_slice)
    }

    /// The typed `$params` of the view at `address` (§10.1). Empty for an
    /// unregistered or parameter-free view.
    #[must_use]
    pub fn view_params(&self, address: &str) -> &[(String, Type)] {
        self.views.get(address).map_or(&[], Vec::as_slice)
    }

    /// The declared `$credential` type of authenticator `auth` (§11.3), if the
    /// schema carries it.
    #[must_use]
    pub fn credential(&self, auth: &str) -> Option<&Type> {
        self.credentials.get(auth)
    }

    /// The keyring authenticator `auth` verifies against, when it is a native-cose
    /// `$verify: "cose.verify(/ring, …)"` authenticator (§17.7). `None` for any
    /// other authenticator, whose credential decodes through the ordinary typed
    /// [`credential`](Self::credential) path instead of the cose verify gate.
    #[must_use]
    pub fn cose_ring(&self, auth: &str) -> Option<&str> {
        self.cose_rings.get(auth).map(String::as_str)
    }
}

/// Assembles a [`Schema`] from an app's typed contracts.
#[derive(Debug, Default)]
pub struct SchemaBuilder {
    schema: Schema,
}

impl SchemaBuilder {
    /// Register the typed argument contract of a call address (its receiver keys
    /// and parameters, in the order the client's argument object is decoded).
    #[must_use]
    pub fn call(
        mut self,
        address: impl Into<String>,
        args: impl IntoIterator<Item = (String, Type)>,
    ) -> Self {
        self.schema.calls.insert(address.into(), args.into_iter().collect());
        self
    }

    /// Register the typed `$params` of a view address (§10.1). A parameter-free
    /// view is registered with an empty set so a client that sends parameters for it
    /// is rejected.
    #[must_use]
    pub fn view(
        mut self,
        address: impl Into<String>,
        params: impl IntoIterator<Item = (String, Type)>,
    ) -> Self {
        self.schema.views.insert(address.into(), params.into_iter().collect());
        self
    }

    /// Register an authenticator's declared `$credential` type (§11.3).
    #[must_use]
    pub fn credential(mut self, auth: impl Into<String>, ty: Type) -> Self {
        self.schema.credentials.insert(auth.into(), ty);
        self
    }

    /// Register a native-cose authenticator (§17.7): `auth` names a
    /// `$verify: "cose.verify(/ring, $credential)"` authenticator whose wire
    /// credential the connector gates through keyring `ring`'s cose verify before
    /// it reaches the surface authenticator. Registered instead of a scalar
    /// [`credential`](Self::credential) type — the credential is a signed token,
    /// not a client-shaped scalar.
    #[must_use]
    pub fn cose(mut self, auth: impl Into<String>, ring: impl Into<String>) -> Self {
        self.schema.cose_rings.insert(auth.into(), ring.into());
        self
    }

    /// Finish the schema.
    #[must_use]
    pub fn build(self) -> Schema {
        self.schema
    }
}
