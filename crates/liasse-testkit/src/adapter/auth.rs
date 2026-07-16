//! Deriving the surface authentication wiring from a package's `$auth`/`$roles`.
//!
//! The surface layer supplies every §11 mechanism (a [`Verifier`] for `$verify`,
//! a [`RowSource`] for `$actor`/`$members`, a [`Role`] for §10.3 membership), but
//! it takes them as explicit host wiring rather than reading them out of the
//! model — the model validates the declarations and leaves their execution a
//! documented seam. This module reconstructs that wiring for the shape a
//! host-free case declares: a `$verify: "$credential"` authenticator (the proof
//! is the credential itself) whose `$actor` selects a row from a collection, and
//! a role whose `$members` is an inline row-stream expression.
//!
//! `$actor`/`$members` are inline expressions, not named views, yet the
//! surface [`RowSource`] resolves through the engine's named-view API (its own
//! documented stand-in for a `/collection[$key]` selection). So for each such
//! expression this module names a *synthetic view* — the same `accounts_view` /
//! `members_view` a production host declares by hand — to be injected into the
//! model before load; the [`RowSource`] then reads it by name.
//!
//! Out of scope this phase (left unwired, so their roles resolve `denied` exactly
//! as before): an authenticator whose `$verify` calls a host namespace, a
//! `$session`-backed authenticator, and a role `$view` or inline surface `$mut`
//! (the surface router binds only declared mutations).

use liasse_surface::{
    Claims, Credential, Role, RowSource, SessionAuthenticator, Verifier, VerifyFailure,
};
use liasse_value::Value;
use serde_json::Value as J;

/// A `$verify: "$credential"` verifier: the proof *is* the credential. It binds
/// the proof to its authenticator and carries the credential as the account
/// claim, so a stateless authenticator resolves the actor straight from it
/// (§11.3). A non-text credential fails verification (a malformed token).
struct LiteralVerifier {
    auth: String,
}

impl Verifier for LiteralVerifier {
    fn verify(&self, credential: &Credential) -> Result<Claims, VerifyFailure> {
        match credential.value() {
            Value::Text(_) => Ok(Claims::new(&self.auth, None, Some(credential.value().clone()))),
            _ => Err(VerifyFailure::new("credential is not a text token")),
        }
    }
}

/// One host-free authenticator the plan will register: its name and the
/// synthetic view (plus key field) that resolves its `$actor`.
struct AuthnSpec {
    name: String,
    actor_view: String,
    actor_key: String,
}

/// One role the plan will register: its name, the authenticator names it accepts,
/// and the synthetic view (plus key field) that decides its `$members`.
struct RoleSpec {
    name: String,
    accepts: Vec<String>,
    members_view: String,
    members_key: String,
}

/// The reconstructed authentication wiring for a package: the synthetic views to
/// inject before load, and the authenticator/role specs to register on the
/// router. Only host-free shapes appear; everything else is absent, leaving the
/// unwired behavior unchanged.
#[derive(Default)]
pub struct AuthPlan {
    /// `(view name, view expression)` pairs to inject as `$model` views.
    synthetic_views: Vec<(String, String)>,
    authenticators: Vec<AuthnSpec>,
    roles: Vec<RoleSpec>,
}

impl AuthPlan {
    /// Analyze a package's `$auth`/`$roles`, producing the host-free wiring.
    #[must_use]
    pub fn derive(package: &J) -> Self {
        let mut plan = Self::default();
        let Some(model) = package.get("$model") else { return plan };
        let auth = model.get("$auth").and_then(J::as_object);
        let roles = model.get("$roles").and_then(J::as_object);

        if let Some(auth) = auth {
            for (name, definition) in auth {
                plan.plan_authenticator(model, name, definition);
            }
        }
        if let Some(roles) = roles {
            for (name, definition) in roles {
                plan.plan_role(model, name, definition);
            }
        }
        plan
    }

    /// Whether any host-free authenticator/role was wired.
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.authenticators.is_empty()
    }

    /// The synthetic views to inject into `$model` before load.
    pub fn synthetic_views(&self) -> impl Iterator<Item = &(String, String)> {
        self.synthetic_views.iter()
    }

    /// Whether the plan registered an authenticator named `name` (so its role can
    /// safely be wired).
    #[must_use]
    fn has_authenticator(&self, name: &str) -> bool {
        self.authenticators.iter().any(|spec| spec.name == name)
    }

    fn plan_authenticator(&mut self, model: &J, name: &str, definition: &J) {
        // Only the host-free shape: `$verify: "$credential"`, no `$session`.
        let verify = definition.get("$verify").and_then(J::as_str);
        if verify != Some("$credential") || definition.get("$session").is_some() {
            return;
        }
        let Some(actor) = definition.get("$actor").and_then(J::as_str) else { return };
        let Some(collection) = selection_collection(actor) else { return };
        let Some(key) = collection_key(model, collection) else { return };

        let actor_view = format!("liasse_auth_actor_{name}");
        self.synthetic_views.push((actor_view.clone(), format!(".{collection} {{ {key} }}")));
        self.authenticators.push(AuthnSpec {
            name: name.to_owned(),
            actor_view,
            actor_key: key,
        });
    }

    fn plan_role(&mut self, model: &J, name: &str, definition: &J) {
        let accepts = accepted_authenticators(definition);
        // Wire the role only if every authenticator it accepts was wired
        // host-free; otherwise its authenticator selection cannot be honored and
        // the role stays unbound (denied), unchanged from before.
        if accepts.is_empty() || !accepts.iter().all(|auth| self.has_authenticator(auth)) {
            return;
        }
        let Some(members) = definition.get("$members").and_then(J::as_str) else { return };
        // A `$members` that reads a request-scoped variable (`$actor`, a scoped
        // role) cannot be evaluated as a plain named view; leave such a role
        // unwired (denied), unchanged from before, rather than inject an
        // invalid view that would fail the whole package's load.
        if members.contains('$') {
            return;
        }
        let Some(collection) = stream_collection(members) else { return };
        let Some(key) = collection_key(model, collection) else { return };

        let members_view = format!("liasse_role_members_{name}");
        self.synthetic_views.push((members_view.clone(), format!("{members} {{ {key} }}")));
        self.roles.push(RoleSpec {
            name: name.to_owned(),
            accepts,
            members_view,
            members_key: key,
        });
    }

    /// The authenticators this plan wires, as constructed surface objects.
    pub fn authenticators(&self) -> Vec<Box<dyn liasse_surface::Authenticator>> {
        self.authenticators
            .iter()
            .map(|spec| {
                let verifier = Box::new(LiteralVerifier { auth: spec.name.clone() });
                let accounts = RowSource::new(spec.actor_view.clone(), spec.actor_key.clone());
                Box::new(SessionAuthenticator::stateless(spec.name.clone(), verifier, accounts))
                    as Box<dyn liasse_surface::Authenticator>
            })
            .collect()
    }

    /// The roles this plan wires (the surface bindings are assembled separately
    /// from the raw `$roles` block).
    pub fn roles(&self) -> Vec<Role> {
        self.roles
            .iter()
            .map(|spec| {
                Role::new(
                    spec.name.clone(),
                    spec.accepts.clone(),
                    RowSource::new(spec.members_view.clone(), spec.members_key.clone()),
                )
            })
            .collect()
    }
}

/// The `$key` field name of collection `name`, read from the raw model. A
/// composite key is out of scope here (a single-field key is what the actor/
/// member selections use).
fn collection_key(model: &J, name: &str) -> Option<String> {
    match model.get(name)?.get("$key")? {
        J::String(key) => Some(key.clone()),
        _ => None,
    }
}

/// The authenticator names a role's `$auth` accepts: a single string or a list
/// (§11.4 accepts-any-listed).
fn accepted_authenticators(role: &J) -> Vec<String> {
    match role.get("$auth") {
        Some(J::String(name)) => vec![name.clone()],
        Some(J::Array(list)) => list.iter().filter_map(J::as_str).map(ToOwned::to_owned).collect(),
        _ => Vec::new(),
    }
}

/// The collection a `/collection[selector]` `$actor` selection addresses.
fn selection_collection(expr: &str) -> Option<&str> {
    let rest = expr.trim().strip_prefix('/')?;
    let end = rest.find('[').unwrap_or(rest.len());
    let name = rest.get(..end)?.trim();
    (!name.is_empty() && is_identifier(name)).then_some(name)
}

/// The collection a `.collection[...]`/`.collection {...}` row-stream `$members`
/// expression reads from.
fn stream_collection(expr: &str) -> Option<&str> {
    let rest = expr.trim().strip_prefix('.')?;
    let end = rest.find(['[', '{', ' ']).unwrap_or(rest.len());
    let name = rest.get(..end)?.trim();
    (!name.is_empty() && is_identifier(name)).then_some(name)
}

/// Whether `text` is a bare `[A-Za-z_][A-Za-z0-9_]*` identifier.
fn is_identifier(text: &str) -> bool {
    let mut bytes = text.bytes();
    matches!(bytes.next(), Some(b) if b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
