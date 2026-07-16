//! Surface resolution: route a dotted address through the model's *exposed*
//! surfaces only (SPEC.md §10.1, §10.4, §12.1 step 1).
//!
//! The router is the exposure boundary. It holds only public and role surfaces
//! the model actually exposes — a [`SurfaceRouterBuilder`] re-validates every
//! binding against [`liasse_model::Model::surfaces`] and rejects a binding onto
//! an unexposed surface, an undeclared call, or a non-existent mutation/view. A
//! name that is not a bound member — an internal mutation, a mistyped surface, a
//! role the router does not carry — resolves to nothing and is [`Denial`]'d
//! `Unresolved`, so internal declarations are unreachable and only exposed
//! members are callable (§10.1, `red/internal-declarations-not-addressable`).

mod build;

pub use build::{RouterError, SurfaceRouterBuilder};

use std::collections::BTreeMap;

use crate::address::{Authority, SurfaceAddress};
use crate::authn::Authenticator;
use crate::binding::{CallBinding, SurfaceBinding, ViewBinding};
use crate::outcome::{Denial, DenialReason};
use crate::role::Role;

/// A role and the surfaces granted to it (§10.3).
struct RoleGrant {
    role: Role,
    surfaces: BTreeMap<String, SurfaceBinding>,
}

/// The resolved routing target of an address.
pub enum Resolved<'a> {
    /// A public surface `$view` (no actor).
    PublicView(&'a ViewBinding),
    /// A public surface `$mut` call (no actor).
    PublicCall(&'a CallBinding),
    /// A role surface `$view`, gated by `role`.
    RoleView { role: &'a Role, binding: &'a ViewBinding },
    /// A role surface `$mut` call, gated by `role`.
    RoleCall { role: &'a Role, binding: &'a CallBinding },
}

/// The exposed external surface of a package: public surfaces, role surfaces,
/// and the authenticators the roles select.
pub struct SurfaceRouter {
    public: BTreeMap<String, SurfaceBinding>,
    roles: BTreeMap<String, RoleGrant>,
    authenticators: BTreeMap<String, Box<dyn Authenticator>>,
}

impl SurfaceRouter {
    /// Resolve a dotted address to its routing target, or deny it as
    /// unresolvable (§12.1 step 1). Every not-exposed outcome — unknown surface,
    /// unknown call, unknown role — is one `Unresolved` denial, so a
    /// nonexistent name is indistinguishable from an ungranted one (§10.1,
    /// SPEC-ISSUES item 8).
    ///
    /// # Errors
    /// Returns a [`Denial`] when the address names nothing exposed.
    pub fn resolve(&self, address: &SurfaceAddress) -> Result<Resolved<'_>, Denial> {
        match address.authority() {
            Authority::Public => self.resolve_public(address),
            Authority::Role(role) => self.resolve_role(role, address),
        }
    }

    fn resolve_public(&self, address: &SurfaceAddress) -> Result<Resolved<'_>, Denial> {
        let surface = self.public.get(address.surface()).ok_or_else(Self::unresolved)?;
        Self::pick_public(surface, address)
    }

    fn resolve_role(&self, role: &str, address: &SurfaceAddress) -> Result<Resolved<'_>, Denial> {
        let grant = self.roles.get(role).ok_or_else(Self::unresolved)?;
        let surface = grant.surfaces.get(address.surface()).ok_or_else(Self::unresolved)?;
        match address.call() {
            Some(call) => {
                let binding = surface.call(call).ok_or_else(Self::unresolved)?;
                Ok(Resolved::RoleCall { role: &grant.role, binding })
            }
            None => {
                let binding = surface.view().ok_or_else(Self::unresolved)?;
                Ok(Resolved::RoleView { role: &grant.role, binding })
            }
        }
    }

    fn pick_public<'a>(
        surface: &'a SurfaceBinding,
        address: &SurfaceAddress,
    ) -> Result<Resolved<'a>, Denial> {
        match address.call() {
            Some(call) => surface.call(call).map(Resolved::PublicCall).ok_or_else(Self::unresolved),
            None => surface.view().map(Resolved::PublicView).ok_or_else(Self::unresolved),
        }
    }

    fn unresolved() -> Denial {
        Denial::new(DenialReason::Unresolved, "the address names no exposed surface")
    }

    /// The authenticator named `name`, if the router carries it (§11.4).
    #[must_use]
    pub fn authenticator(&self, name: &str) -> Option<&dyn Authenticator> {
        self.authenticators.get(name).map(AsRef::as_ref)
    }

    /// The role named `name`, if the router carries it.
    #[must_use]
    pub fn role(&self, name: &str) -> Option<&Role> {
        self.roles.get(name).map(|grant| &grant.role)
    }

    /// The public surface names, in canonical order (for the manifest, §12.1).
    pub fn public_surfaces(&self) -> impl Iterator<Item = &String> {
        self.public.keys()
    }

    /// The role names the router carries, in canonical order (for the manifest).
    pub fn role_names(&self) -> impl Iterator<Item = &String> {
        self.roles.keys()
    }

    /// The surface names granted to the role named `role`, in canonical order.
    pub fn role_surfaces(&self, role: &str) -> impl Iterator<Item = &String> {
        self.roles.get(role).into_iter().flat_map(|grant| grant.surfaces.keys())
    }
}
