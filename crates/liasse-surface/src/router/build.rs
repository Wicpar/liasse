//! Building a [`SurfaceRouter`] and re-validating it against the model's exposed
//! surface (§10.1, §10.4).
//!
//! A router that binds a surface the model does not expose, a call the surface
//! does not declare, or a mutation/view the package does not define would let a
//! client reach past the model's exposure boundary. The builder rejects each of
//! those at assembly, so a constructed [`SurfaceRouter`] is proof every route
//! lands on an exposed, declared member — parse, don't validate.

use std::collections::BTreeMap;

use liasse_model::{Model, Node};

use crate::authn::Authenticator;
use crate::binding::SurfaceBinding;
use crate::role::Role;

use super::{RoleGrant, SurfaceRouter};

/// A role registered in a builder together with the surfaces granted to it.
struct PendingRole {
    role: Role,
    surfaces: BTreeMap<String, SurfaceBinding>,
}

/// Assembles a [`SurfaceRouter`], validating against the model at [`build`].
///
/// [`build`]: SurfaceRouterBuilder::build
#[derive(Default)]
pub struct SurfaceRouterBuilder {
    public: BTreeMap<String, SurfaceBinding>,
    roles: BTreeMap<String, PendingRole>,
    authenticators: BTreeMap<String, Box<dyn Authenticator>>,
}

impl SurfaceRouterBuilder {
    /// An empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Expose the public surface `name` with `binding` (§10.2).
    #[must_use]
    pub fn public_surface(mut self, name: impl Into<String>, binding: SurfaceBinding) -> Self {
        self.public.insert(name.into(), binding);
        self
    }

    /// Register the authenticator `authenticator` under its own name (§11.3).
    #[must_use]
    pub fn authenticator(mut self, authenticator: Box<dyn Authenticator>) -> Self {
        self.authenticators.insert(authenticator.name().to_owned(), authenticator);
        self
    }

    /// Register `role` and the surfaces granted to it (§10.3).
    #[must_use]
    pub fn role(
        mut self,
        role: Role,
        surfaces: impl IntoIterator<Item = (String, SurfaceBinding)>,
    ) -> Self {
        let name = role.name().to_owned();
        self.roles.insert(name, PendingRole { role, surfaces: surfaces.into_iter().collect() });
        self
    }

    /// Validate every binding against `model`'s exposed surfaces and declared
    /// members, producing the router.
    ///
    /// # Errors
    /// Returns a [`RouterError`] for a surface the model does not expose, a call
    /// the exposed surface does not declare, a mutation or view the package does
    /// not define, or a role selecting an unregistered authenticator.
    pub fn build(self, model: &Model) -> Result<SurfaceRouter, RouterError> {
        let exposure = Exposure::new(model);
        for (name, surface) in &self.public {
            exposure.check_surface(name, surface, true)?;
        }
        for grant in self.roles.values() {
            for auth in grant.role.accepted_names() {
                if !self.authenticators.contains_key(auth) {
                    return Err(RouterError::UnknownAuthenticator(auth.to_owned()));
                }
            }
            exposure.check_view_exists(grant.role.members().view())?;
            for (name, surface) in &grant.surfaces {
                exposure.check_surface(name, surface, false)?;
            }
        }
        let roles = self
            .roles
            .into_iter()
            .map(|(name, pending)| (name, RoleGrant { role: pending.role, surfaces: pending.surfaces }))
            .collect();
        Ok(SurfaceRouter { public: self.public, roles, authenticators: self.authenticators })
    }
}

/// The set of names the model exposes and declares, indexed for validation.
struct Exposure<'a> {
    model: &'a Model,
}

impl<'a> Exposure<'a> {
    fn new(model: &'a Model) -> Self {
        Self { model }
    }

    /// Whether the model exposes a surface of `name` with the given publicity,
    /// and whether that surface declares the external `call` name.
    fn exposes_call(&self, name: &str, public: bool, call: &str) -> bool {
        self.model.surfaces().iter().any(|surface| {
            surface.public == public
                && surface.name.as_str() == name
                && surface.calls.iter().any(|c| c.as_str() == call)
        })
    }

    fn exposes_surface(&self, name: &str, public: bool) -> bool {
        self.model.surfaces().iter().any(|surface| surface.public == public && surface.name.as_str() == name)
    }

    fn declares_mutation(&self, name: &str) -> bool {
        self.model.mutations().iter().any(|mutation| mutation.name.as_str() == name)
    }

    fn declares_view(&self, name: &str) -> bool {
        self.model
            .root()
            .members
            .iter()
            .any(|member| matches!(member.node, Node::View(_)) && member.name.as_str() == name)
    }

    fn check_view_exists(&self, view: &str) -> Result<(), RouterError> {
        if self.declares_view(view) {
            Ok(())
        } else {
            Err(RouterError::UnknownView(view.to_owned()))
        }
    }

    /// Validate one surface binding: it must be exposed, and each of its members
    /// must be both exposed and backed by a declared mutation/view.
    fn check_surface(
        &self,
        name: &str,
        surface: &SurfaceBinding,
        public: bool,
    ) -> Result<(), RouterError> {
        if !self.exposes_surface(name, public) {
            return Err(RouterError::UnexposedSurface(name.to_owned()));
        }
        if let Some(view) = surface.view()
            && !view.is_surface()
        {
            // A surface-view binding names the runtime's compiled surface view by
            // its dotted address (§10.1); its existence is proven by this surface's
            // own exposure (checked above), not by a declared top-level view.
            self.check_view_exists(view.view())?;
        }
        for call in surface.call_names() {
            if !self.exposes_call(name, public, call) {
                return Err(RouterError::UnexposedCall { surface: name.to_owned(), call: call.clone() });
            }
        }
        for call in surface.call_names() {
            let binding = surface.call(call).ok_or_else(|| RouterError::UnexposedCall {
                surface: name.to_owned(),
                call: call.clone(),
            })?;
            if !self.declares_mutation(binding.mutation()) {
                return Err(RouterError::UnknownMutation(binding.mutation().to_owned()));
            }
        }
        Ok(())
    }
}

/// Why a router failed to assemble against a model.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RouterError {
    /// A bound surface is not exposed by the model's `$public`/`$roles` (§10.1).
    #[error("surface `{0}` is not exposed by the model")]
    UnexposedSurface(String),
    /// A bound call is not declared under its surface's `$mut` (§10.1).
    #[error("surface `{surface}` does not expose call `{call}`")]
    UnexposedCall {
        /// The surface the call was bound under.
        surface: String,
        /// The undeclared external call name.
        call: String,
    },
    /// A call binds onto a mutation the package does not declare (§10.4).
    #[error("no mutation named `{0}` is declared")]
    UnknownMutation(String),
    /// A view binding names a view the package does not declare (§10.4).
    #[error("no view named `{0}` is declared")]
    UnknownView(String),
    /// A role selects an authenticator the router does not carry (§11.4).
    #[error("role selects unregistered authenticator `{0}`")]
    UnknownAuthenticator(String),
}
