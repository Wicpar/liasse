//! Deriving the surface authentication wiring from a package's `$auth`/`$roles`
//! and the case's `hosts` block.
//!
//! The surface layer supplies every §11 mechanism (a [`Verifier`] for `$verify`,
//! a [`RowSource`] for `$actor`/`$members`, a [`SessionSource`] for `$session`, a
//! [`Role`] for §10.3 membership), but it takes them as explicit host wiring
//! rather than reading them out of the model — the model validates the
//! declarations and leaves their execution a documented seam. This module
//! reconstructs that wiring for the shapes a conformance case declares:
//!
//! - a `$verify: "$credential"` authenticator (the proof *is* the credential),
//! - a `$verify: "<ns>.<fn>($credential)"` authenticator backed by a host
//!   verifier namespace — the case's `hosts` block declares the credential →
//!   proof table (a `tokens` map, or a verifier `functions.<fn>.accepts` map),
//! - a stateless (`api_key`) authenticator resolving `$actor` straight from the
//!   proof's account claim, and a `$session`-backed authenticator resolving the
//!   `$session` row (§11.2) and then `$actor` from that row (§11.3).
//!
//! `$actor`/`$session`/`$members` are inline expressions, not named views, yet
//! the surface [`RowSource`]/[`SessionSource`] resolve through the engine's
//! named-view API. So for each such expression this module names a *synthetic
//! view* — the `accounts_view` / `sessions_view` / `members_view` a production
//! host declares by hand — to be injected into the model before load; the row
//! sources then read them by name.
//!
//! Out of scope this phase (left unwired, so their roles resolve `denied`
//! exactly as before): a `$verify` calling a host namespace whose proof table is
//! minted at runtime (a `token.sign` login), a role `$members` that reads a
//! request-scoped variable (`$actor`, a scoped role), and a role `$view`/inline
//! surface `$mut` the surface router does not bind.

use std::collections::BTreeMap;

use liasse_surface::{
    Claims, Credential, Role, RowSource, SessionAuthenticator, SessionSource, Verifier,
    VerifyFailure,
};
use liasse_value::{Text, Type, Value};
use serde_json::Value as J;

use crate::hosts::HostsConfig;

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

/// The typed claims a host verifier reads from a credential's declared proof
/// entry (§11.3 `$proof`): the authenticator it is bound to, and the session /
/// account keys it selects, each already decoded to the target collection's key
/// type so a `uuid`/`int` key matches by value, not by spelling.
#[derive(Clone)]
struct HostProof {
    auth: Option<String>,
    session: Option<Value>,
    account: Option<Value>,
}

/// A `$verify: "<ns>.<fn>($credential)"` verifier standing in for a host verifier
/// namespace (§16.3): it looks the credential up in the case's declared proof
/// table and returns its typed [`Claims`]. A credential with no declared entry
/// fails verification (a forged/unknown token); a non-text credential is
/// malformed. The proof's own `auth` claim binds it (§11.4); absent one, the
/// authenticator's own name is used so a single-authenticator `$check` passes.
///
/// When `split` is set, a credential absent from the static table but containing
/// a `:` is verified by splitting it at the first colon into `{ auth, account }`
/// — the deterministic behavioral `authsim.verify` convention the corpus
/// documents (`tests/12-clients-live-views/NOTES.md`).
struct HostVerifier {
    auth: String,
    tokens: BTreeMap<String, HostProof>,
    split: bool,
    account_ty: Type,
}

impl Verifier for HostVerifier {
    fn verify(&self, credential: &Credential) -> Result<Claims, VerifyFailure> {
        let Value::Text(text) = credential.value() else {
            return Err(VerifyFailure::new("credential is not a text token"));
        };
        if let Some(proof) = self.tokens.get(text.as_str()) {
            let auth = proof.auth.clone().unwrap_or_else(|| self.auth.clone());
            return Ok(Claims::new(auth, proof.session.clone(), proof.account.clone()));
        }
        if self.split
            && let Some((auth, account)) = text.as_str().split_once(':')
        {
            let account = decode_key(&J::String(account.to_owned()), &self.account_ty);
            return Ok(Claims::new(auth, None, Some(account)));
        }
        Err(VerifyFailure::new("no proof matches the credential"))
    }
}

/// A verifier for a `$verify: "cose.verify(/ring, $credential)"` authenticator
/// (§17.7/§17.8). The cose token is verified against the ring's accepted versions
/// *before* this runs — at the adapter's auth layer, through
/// [`Engine::cose_verify`](liasse_runtime::Engine::cose_verify), an acceptance
/// read this surface [`Verifier`] seam cannot reach the engine to perform. So the
/// credential this receives is the already-verified claims struct (a login-minted
/// token that verifies) or a non-struct sentinel (a wrong-keyring / rotated-out /
/// revoked token that did not). This only decodes the verified claims to typed
/// [`Claims`]: the `auth` claim binds the token to its authenticator (§11.4), and
/// the `session`/`account` claims select the session and account rows (§11.3).
struct CoseVerifier;

impl Verifier for CoseVerifier {
    fn verify(&self, credential: &Credential) -> Result<Claims, VerifyFailure> {
        let Value::Struct(claims) = credential.value() else {
            return Err(VerifyFailure::new("cose token did not verify against the keyring"));
        };
        let Some(Value::Text(auth)) = claims.get("auth") else {
            return Err(VerifyFailure::new("verified cose claims carry no `auth` binding"));
        };
        Ok(Claims::new(auth.as_str(), claims.get("session").cloned(), claims.get("account").cloned()))
    }
}

/// Which verifier a planned authenticator uses.
enum VerifierSpec {
    /// `$verify: "$credential"` — the proof is the credential.
    Literal,
    /// A host verifier namespace: its declared credential → proof table, whether
    /// a colon-split behavioral fallback applies, and the account key type.
    Host { tokens: BTreeMap<String, HostProof>, split: bool, account_ty: Type },
    /// `$verify: "cose.verify(/ring, $credential)"` — the credential is a cose
    /// token gated through [`Engine::cose_verify`](liasse_runtime::Engine::cose_verify)
    /// at the adapter's auth layer before it reaches [`CoseVerifier`].
    Cose,
}

/// The `$session` wiring of a session-backed authenticator (§11.2): the synthetic
/// session view and the field names that carry the account reference, the expiry
/// instant, and the revocation flag.
struct SessionWiring {
    view: String,
    key_field: String,
    account_field: String,
    expires_field: String,
    revoked_field: String,
}

/// One authenticator the plan will register: its name, the synthetic view (plus
/// key field) that resolves its `$actor`, its optional `$session` wiring, and its
/// verifier.
struct AuthnSpec {
    name: String,
    actor_view: String,
    actor_key: String,
    session: Option<SessionWiring>,
    verifier: VerifierSpec,
}

/// One role the plan will register: its name, the authenticator names it accepts,
/// and the synthetic view (plus key field) that decides its `$members`.
struct RoleSpec {
    name: String,
    accepts: Vec<String>,
    members_view: String,
    members_key: String,
    /// The collection the role is nested on when it is a SCOPED role (§10.3/§10.5):
    /// its surfaces read `.` as a row of that collection, addressed by the request
    /// scope. `None` for a package-level role, whose `.` is the package root.
    scope: Option<String>,
}

/// The reconstructed authentication wiring for a package: the synthetic views to
/// inject before load, and the authenticator/role specs to register on the
/// router. A shape this module does not model is absent, leaving the unwired
/// behavior (`denied`) unchanged.
#[derive(Default)]
pub struct AuthPlan {
    /// `(view name, view expression)` pairs to inject as `$model` views.
    synthetic_views: Vec<(String, String)>,
    authenticators: Vec<AuthnSpec>,
    roles: Vec<RoleSpec>,
    /// `(authenticator name, keyring name)` for each `$verify: "cose.verify(/ring,
    /// $credential)"` authenticator (§17.7), so the adapter's auth layer gates its
    /// credential through [`Engine::cose_verify`](liasse_runtime::Engine::cose_verify)
    /// against the named ring before the surface authenticator resolves it.
    cose_rings: Vec<(String, String)>,
}

impl AuthPlan {
    /// Analyze a package's `$auth`/`$roles` against the case's `hosts` block,
    /// producing the reconstructed wiring.
    #[must_use]
    pub fn derive(package: &J, hosts: Option<&J>) -> Self {
        let mut plan = Self::default();
        let Some(model) = package.get("$model") else { return plan };
        let auth = model.get("$auth").and_then(J::as_object);
        let roles = model.get("$roles").and_then(J::as_object);

        if let Some(auth) = auth {
            for (name, definition) in auth {
                plan.plan_authenticator(model, hosts, name, definition);
            }
        }
        if let Some(roles) = roles {
            for (name, definition) in roles {
                plan.plan_role(model, name, definition);
            }
        }
        // §10.3/§10.5: a role nested on a collection row is a scoped role. Walk each
        // top-level collection's `$roles` and wire those separately, recording the
        // scope collection so the covered `$view` reads the addressed row.
        if let Some(collections) = model.as_object() {
            for (collection, shape) in collections {
                if collection.starts_with('$') {
                    continue;
                }
                let Some(nested) = shape.get("$roles").and_then(J::as_object) else { continue };
                for (name, definition) in nested {
                    plan.plan_scoped_role(model, collection, name, definition);
                }
            }
        }
        plan
    }

    /// The collection a scoped role registered under `name` is nested on (§10.5),
    /// so the router can bind its surfaces and decode a subscription's scope key.
    #[must_use]
    pub fn scoped_role_collection(&self, name: &str) -> Option<&str> {
        self.roles
            .iter()
            .find(|spec| spec.name == name)
            .and_then(|spec| spec.scope.as_deref())
    }

    /// Whether any authenticator was wired.
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.authenticators.is_empty()
    }

    /// Each `$verify: "cose.verify(/ring, …)"` authenticator's `(name, ring)`
    /// pair (§17.7), so the adapter records which authenticator credentials to
    /// gate through [`Engine::cose_verify`](liasse_runtime::Engine::cose_verify)
    /// and against which keyring.
    pub fn cose_authenticators(&self) -> impl Iterator<Item = &(String, String)> {
        self.cose_rings.iter()
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

    fn plan_authenticator(&mut self, model: &J, hosts: Option<&J>, name: &str, definition: &J) {
        let Some(verify) = definition.get("$verify").and_then(J::as_str) else { return };
        let Some(actor) = definition.get("$actor").and_then(J::as_str) else { return };
        let Some(actor_collection) = selection_collection(actor) else { return };
        let Some(actor_key) = collection_key(model, actor_collection) else { return };
        let account_ty = collection_key_type(model, actor_collection);

        let is_literal = verify == "$credential";
        let cose_ring = cose_verify_ring(verify);

        // `$session` wiring. A `$verify: "$credential"` proof carries no session
        // claim, so a `$session` with the literal verifier stays unwired.
        let session = match definition.get("$session").and_then(J::as_str) {
            Some(_) if is_literal => return,
            Some(expr) => {
                let Some(collection) = selection_collection(expr) else { return };
                let Some(fields) = session_fields(model, collection, actor) else { return };
                Some((collection.to_owned(), fields))
            }
            None => None,
        };

        let verifier = if is_literal {
            VerifierSpec::Literal
        } else if let Some(ring) = &cose_ring {
            // §17.7: the token is gated through `Engine::cose_verify` at the auth
            // layer; record the ring so that gating targets the right keyring.
            self.cose_rings.push((name.to_owned(), ring.clone()));
            VerifierSpec::Cose
        } else {
            let session_ty = session
                .as_ref()
                .map_or(Type::Text, |(collection, _)| collection_key_type(model, collection));
            VerifierSpec::Host {
                tokens: host_proofs(hosts, &session_ty, &account_ty),
                split: hosts_have_behavioral_verifier(hosts),
                account_ty: account_ty.clone(),
            }
        };

        let actor_view = format!("liasse_auth_actor_{name}");
        self.synthetic_views
            .push((actor_view.clone(), format!(".{actor_collection} {{ {actor_key} }}")));

        let session = session.map(|(collection, fields)| {
            let view = format!("liasse_auth_session_{name}");
            self.synthetic_views
                .push((view.clone(), format!(".{collection} {{ {} }}", fields.projection())));
            let expires_field = fields.expires_field();
            let revoked_field = fields.revoked_field();
            SessionWiring {
                view,
                key_field: fields.key,
                account_field: fields.account,
                expires_field,
                revoked_field,
            }
        });

        self.authenticators.push(AuthnSpec {
            name: name.to_owned(),
            actor_view,
            actor_key,
            session,
            verifier,
        });
    }

    fn plan_role(&mut self, model: &J, name: &str, definition: &J) {
        let accepts = accepted_authenticators(definition);
        // Wire the role only if every authenticator it accepts was wired;
        // otherwise its authenticator selection cannot be honored and the role
        // stays unbound (denied), unchanged from before.
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
            scope: None,
        });
    }

    /// §10.3/§10.5: wire a role nested on a collection row (a SCOPED role). Its
    /// `$members` reads `.` as the role-holding row (`.members[:m | m.admin]…`), so
    /// membership is reconstructed as the flattened view of that relation across the
    /// scope collection (`.<coll>[:__c].members[:m | m.admin] { <key> }`). Scope-row
    /// admission (denying a watch on a row the actor does not hold) is threaded with
    /// the deferred §10.5 addressing; this phase authorizes any holder of the role
    /// and lets the covered `$view` project the scope row the subscription names.
    fn plan_scoped_role(&mut self, model: &J, scope: &str, name: &str, definition: &J) {
        let accepts = accepted_authenticators(definition);
        if accepts.is_empty() || !accepts.iter().all(|auth| self.has_authenticator(auth)) {
            return;
        }
        let Some(members) = definition.get("$members").and_then(J::as_str) else { return };
        // A `$members` reading a request-scoped `$actor` is a further seam; skip it.
        if members.contains('$') {
            return;
        }
        // The `$members` relation is rooted at `.` (the scope row). Its member
        // collection is the leading segment; the actor key it projects is that
        // collection's `$key` (`account`), so drop a trailing `.<key>` navigation
        // and project the key field directly.
        let Some(relation) = stream_collection(members) else { return };
        let Some(key) = nested_collection_key(model, scope, relation) else { return };
        let stream = members.strip_suffix(&format!(".{key}")).unwrap_or(members);
        let members_view = format!("liasse_role_members_{name}");
        self.synthetic_views
            .push((members_view.clone(), format!(".{scope}[:__scope_c]{stream} {{ {key} }}")));
        self.roles.push(RoleSpec {
            name: name.to_owned(),
            accepts,
            members_view,
            members_key: key,
            scope: Some(scope.to_owned()),
        });
    }

    /// The authenticators this plan wires, as constructed surface objects.
    pub fn authenticators(&self) -> Vec<Box<dyn liasse_surface::Authenticator>> {
        self.authenticators
            .iter()
            .map(|spec| {
                let verifier: Box<dyn Verifier> = match &spec.verifier {
                    VerifierSpec::Literal => Box::new(LiteralVerifier { auth: spec.name.clone() }),
                    VerifierSpec::Host { tokens, split, account_ty } => Box::new(HostVerifier {
                        auth: spec.name.clone(),
                        tokens: tokens.clone(),
                        split: *split,
                        account_ty: account_ty.clone(),
                    }),
                    VerifierSpec::Cose => Box::new(CoseVerifier),
                };
                let accounts = RowSource::new(spec.actor_view.clone(), spec.actor_key.clone());
                match &spec.session {
                    Some(session) => {
                        let source = SessionSource::new(
                            RowSource::new(session.view.clone(), session.key_field.clone()),
                            session.account_field.clone(),
                            session.expires_field.clone(),
                            session.revoked_field.clone(),
                        );
                        Box::new(SessionAuthenticator::session(
                            spec.name.clone(),
                            verifier,
                            source,
                            accounts,
                        )) as Box<dyn liasse_surface::Authenticator>
                    }
                    None => Box::new(SessionAuthenticator::stateless(
                        spec.name.clone(),
                        verifier,
                        accounts,
                    )) as Box<dyn liasse_surface::Authenticator>,
                }
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

/// The keyring a `$verify: "cose.verify(/ring, $credential)"` authenticator
/// verifies against (§17.7), read from the verify expression. `None` for any
/// other `$verify` shape.
fn cose_verify_ring(verify: &str) -> Option<String> {
    let inner = verify.trim().strip_prefix("cose.verify(")?;
    let first = inner.split(',').next()?.trim();
    let ring = first.trim_start_matches('/').trim();
    (!ring.is_empty()).then(|| ring.to_owned())
}

/// The `$session` collection's field roles (§11.2): its key, the account
/// reference field, and — when declared — the expiry instant and revocation
/// flag. A session collection omitting the expiry field (`expires`/bucket upper
/// bound) leaves it `None`; the surface [`SessionSource`] then reads a missing
/// expiry as an unbounded (perpetual) session — active until revoked (§11.7,
/// §14 omitted upper bound) — rather than denying, so a keyring package that
/// isolates the token rule from session expiry authenticates.
struct SessionFields {
    key: String,
    account: String,
    expires: Option<String>,
    revoked: Option<String>,
}

impl SessionFields {
    /// The session view projection: the key and account, plus the lifetime
    /// fields that actually exist (projecting an absent field would fail model
    /// compilation).
    fn projection(&self) -> String {
        let mut fields = vec![self.key.clone(), self.account.clone()];
        fields.extend(self.expires.clone());
        fields.extend(self.revoked.clone());
        fields.join(", ")
    }

    /// The expiry field name the session source reads — the declared one, or the
    /// §11.2 conventional `expires_at` when the collection declares none (so the
    /// source reads an absent field and denies).
    fn expires_field(&self) -> String {
        self.expires.clone().unwrap_or_else(|| "expires_at".to_owned())
    }

    /// The revocation field name the session source reads — the declared one, or
    /// the §11.2 conventional `revoked` when the collection declares none.
    fn revoked_field(&self) -> String {
        self.revoked.clone().unwrap_or_else(|| "revoked".to_owned())
    }
}

/// The §11.2 session field roles of collection `name`, read from the raw model:
/// its key, the account reference (the `$session.<field>` named by `actor_expr`,
/// else the sole `$ref` field), and the timestamp/bool lifetime fields by type.
fn session_fields(model: &J, name: &str, actor_expr: &str) -> Option<SessionFields> {
    let key = collection_key(model, name)?;
    let members = model.get(name)?.as_object()?;
    let account = session_account_field(actor_expr)
        .filter(|field| members.contains_key(field.as_str()))
        .or_else(|| ref_field(members))?;
    Some(SessionFields {
        key,
        account,
        expires: typed_field(members, "timestamp"),
        revoked: typed_field(members, "bool"),
    })
}

/// The member named by a `$actor` selection's `$session.<field>` account key
/// (§11.3), if the selection reads one.
fn session_account_field(actor_expr: &str) -> Option<String> {
    let rest = actor_expr.split("$session.").nth(1)?;
    let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(rest.len());
    let field = rest.get(..end)?;
    (!field.is_empty()).then(|| field.to_owned())
}

/// The sole member of `collection` declared as a `{ $ref: ... }` field — the
/// session's account reference (§11.2).
fn ref_field(members: &serde_json::Map<String, J>) -> Option<String> {
    members
        .iter()
        .find(|(_, value)| value.get("$ref").is_some())
        .map(|(name, _)| name.clone())
}

/// The first member of `collection` whose scalar declaration starts with `type`
/// (a `timestamp`/`bool` lifetime field, §11.2).
fn typed_field(members: &serde_json::Map<String, J>, ty: &str) -> Option<String> {
    members
        .iter()
        .filter_map(|(name, value)| value.as_str().map(|decl| (name, decl)))
        .find(|(_, decl)| scalar_type_token(decl) == Some(ty))
        .map(|(name, _)| name.clone())
}

/// The credential → proof table declared by the case's `hosts` block: every
/// `tokens` map entry and every verifier `functions.<fn>.accepts` entry, with the
/// session/account keys decoded to the collections' key types.
fn host_proofs(hosts: Option<&J>, session_ty: &Type, account_ty: &Type) -> BTreeMap<String, HostProof> {
    let mut table = BTreeMap::new();
    let Some(hosts) = hosts else { return table };
    for component in &HostsConfig::parse(hosts).components {
        collect_proofs(&component.config, session_ty, account_ty, &mut table);
    }
    table
}

/// Whether the case's `hosts` block declares a *behavioral* verifier function —
/// a namespace `functions.<fn>` with no static `accepts` table — so a host
/// verifier applies the documented `authsim` colon-split fallback. A namespace
/// declaring only static `tokens`/`accepts` maps does not.
fn hosts_have_behavioral_verifier(hosts: Option<&J>) -> bool {
    let Some(hosts) = hosts else { return false };
    HostsConfig::parse(hosts).components.iter().any(|component| {
        component
            .config
            .get("functions")
            .and_then(J::as_object)
            .is_some_and(|functions| functions.values().any(|function| function.get("accepts").is_none()))
    })
}

/// Collect the credential → proof entries a single host component declares, from
/// its `tokens` map and any `functions.<fn>.accepts` map.
fn collect_proofs(config: &J, session_ty: &Type, account_ty: &Type, out: &mut BTreeMap<String, HostProof>) {
    if let Some(tokens) = config.get("tokens").and_then(J::as_object) {
        for (credential, proof) in tokens {
            out.insert(credential.clone(), read_proof(proof, session_ty, account_ty));
        }
    }
    if let Some(functions) = config.get("functions").and_then(J::as_object) {
        for function in functions.values() {
            if let Some(accepts) = function.get("accepts").and_then(J::as_object) {
                for (credential, proof) in accepts {
                    out.insert(credential.clone(), read_proof(proof, session_ty, account_ty));
                }
            }
        }
    }
}

/// One declared proof entry, with its session/account keys decoded to type.
fn read_proof(proof: &J, session_ty: &Type, account_ty: &Type) -> HostProof {
    HostProof {
        auth: proof.get("auth").and_then(J::as_str).map(ToOwned::to_owned),
        session: proof.get("session").map(|wire| decode_key(wire, session_ty)),
        account: proof.get("account").map(|wire| decode_key(wire, account_ty)),
    }
}

/// Decode a wire proof key to `ty`, falling back to the verbatim text when the
/// wire form does not parse as that type (so a mismatch denies at resolution
/// rather than crashing).
fn decode_key(wire: &J, ty: &Type) -> Value {
    ty.decode(wire).unwrap_or_else(|_| {
        Value::Text(Text::new(wire.as_str().map_or_else(|| wire.to_string(), ToOwned::to_owned)))
    })
}

/// The `$key` field name of collection `name`, read from the raw model. A
/// composite key is out of scope here (a single-field key is what the actor/
/// session/member selections use).
fn collection_key(model: &J, name: &str) -> Option<String> {
    match model.get(name)?.get("$key")? {
        J::String(key) => Some(key.clone()),
        _ => None,
    }
}

/// The single-field `$key` of a collection `nested` declared inside collection
/// `parent` (§5.4) — the member-relation key a scoped role's `$members` projects
/// (`companies.members.$key` = `account`). `None` for an absent or composite key.
fn nested_collection_key(model: &J, parent: &str, nested: &str) -> Option<String> {
    match model.get(parent)?.get(nested)?.get("$key")? {
        J::String(key) => Some(key.clone()),
        _ => None,
    }
}

/// The key type of collection `name` (its single-field `$key`), read from the
/// raw field declaration. A collection whose key field is undeclared or composite
/// falls back to `text`.
pub(super) fn collection_key_type(model: &J, name: &str) -> Type {
    let Some(key) = collection_key(model, name) else { return Type::Text };
    let decl = model.get(name).and_then(|collection| collection.get(&key)).and_then(J::as_str);
    decl.map_or(Type::Text, scalar_type)
}

/// The leading type token of a scalar field declaration (`"uuid = uuid()"` →
/// `"uuid"`), if any.
fn scalar_type_token(decl: &str) -> Option<&str> {
    decl.trim()
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .find(|token| !token.is_empty())
}

/// The scalar [`Type`] a field declaration names, defaulting to `text` for any
/// token this reconstruction does not decode a key for.
fn scalar_type(decl: &str) -> Type {
    match scalar_type_token(decl) {
        Some("uuid") => Type::Uuid,
        Some("int") => Type::Int,
        Some("decimal") => Type::Decimal,
        Some("bool") => Type::Bool,
        Some("date") => Type::Date,
        Some("timestamp") => Type::timestamp(),
        Some("bytes") => Type::Bytes,
        Some("duration") => Type::Duration,
        _ => Type::Text,
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

/// The collection a `/collection[selector]` `$actor`/`$session` selection
/// addresses.
fn selection_collection(expr: &str) -> Option<&str> {
    let rest = expr.trim().strip_prefix('/')?;
    let end = rest.find('[').unwrap_or(rest.len());
    let name = rest.get(..end)?.trim();
    (!name.is_empty() && is_identifier(name)).then_some(name)
}

/// The collection a `.collection[...]`/`.collection {...}` row-stream — or a
/// root-absolute `/collection` selection — `$members` expression reads from. Both
/// the current-scope `.` and root `/` prefixes name the same top-level collection
/// at the root, so a `$members: "/accounts"` (all rows) wires the same way as a
/// `$members: ".accounts[…]"` (a filtered stream).
fn stream_collection(expr: &str) -> Option<&str> {
    let trimmed = expr.trim();
    let rest = trimmed.strip_prefix('.').or_else(|| trimmed.strip_prefix('/'))?;
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
