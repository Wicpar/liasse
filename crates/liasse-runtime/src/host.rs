//! Host-component binding and per-call dispatch (§16, §17.7/§17.8).
//!
//! Two concerns live here, kept apart:
//!
//! - [`HostBinding`] is owned by the [`Engine`](crate::Engine): the [`Registry`]
//!   of registered host components and the resolved `$requires` map (a local
//!   expression namespace → the semantic [`ContractRef`] it pins). It is built
//!   once at load, when a missing / incompatible / ambiguous requirement fails
//!   *before activation* (§16.2, §9.2 step 4). The runtime natively implements
//!   the `liasse.cose` contract through its internally-provisioned keyrings, so
//!   the binding seeds a built-in cose namespace when none is registered.
//!
//! - [`HostDispatch`] is the borrowed, per-admission view the interpreter runs a
//!   host-namespace call against. A generic namespace call goes through the
//!   [`ConformanceGuard`] (§16.2/§16.3), so a component returning an off-contract
//!   type or a verifier rejection is a typed [`Rejection`] rather than trusted. A
//!   `cose.sign(/ring, claims)` call is routed to the managed [`Keyring`], which
//!   owns the provider handles: signing exercises the active version, so a §17.9
//!   provider outage rejects the mutation and mints no token.
//!
//! Only the mutation-admission path builds a live dispatch; genesis seeding,
//! view reads, and migration transforms use [`HostDispatch::none`], since a host
//! call in those positions flows through the pure expression checker
//! (liasse-expr), which types only the core language namespaces — resolving a
//! host-namespace call *there* is a documented cross-crate seam.

use std::collections::{BTreeMap, BTreeSet};

use liasse_expr::{EvalError, ExprType, HostEffect, HostOp, HostOrigin};
use liasse_host::{
    cose_descriptor, verify_cose_signature, ConformanceGuard, ContractRef, CoseClaims, CoseToken,
    EffectClass, GuardError, HostNamespace, InvocationFailure, KeyProvider, NamespaceDescriptor,
    Registry, ResolutionError, SignatureError,
};
use liasse_value::{Timestamp, Value};

use crate::engine_provider::EngineKeyProvider;
use crate::error::{EngineError, Rejection, RejectionReason};
use crate::keyring::{Keyring, VersionId};

/// The resolved `$requires` namespaces' function signatures for the expression
/// checker (§16.2), keyed by local namespace then function. The built-in cose
/// contract is excluded — its `sign`/`verify` are served through the managed
/// keyring, not value-callable host ops — so a view/default host call type-checks
/// only against a package's declared, value-callable namespaces.
#[derive(Debug, Default, Clone)]
pub(crate) struct HostSignatures(BTreeMap<String, BTreeMap<String, HostOp>>);

impl HostSignatures {
    /// The pinned op of `namespace.function`, if the package declares it.
    pub(crate) fn op(&self, namespace: &str, function: &str) -> Option<&HostOp> {
        self.0.get(namespace)?.get(function)
    }

    /// The same resolved signatures as a [`liasse_model::HostDescriptors`], so the
    /// model's Phase-2 checker (`check_tree`) types a `$view`/`$default`/computed
    /// host call against the identical pinned contracts the runtime's own checker
    /// uses (§16.2) — closing the seam where `Model::build` rejected the call as an
    /// unknown function before the runtime's checker ran.
    pub(crate) fn descriptors(&self) -> liasse_model::HostDescriptors {
        liasse_model::HostDescriptors::new(self.0.clone())
    }
}

/// Translate a §16.3 [`EffectClass`] into the expr checker's [`HostEffect`].
const fn effect_of(effect: EffectClass) -> HostEffect {
    match effect {
        EffectClass::Pure => HostEffect::Pure,
        EffectClass::Verifier => HostEffect::Verifier,
        EffectClass::Generated => HostEffect::Generated,
    }
}

/// The semantic contract the runtime implements natively through its
/// internally-provisioned keyrings (§17.7/§17.8), so a package requiring it
/// resolves without an externally registered component.
const COSE_CONTRACT: &str = "liasse.cose";

/// The built-in core-codec contracts (§16.1/§20): the engine links them, so their
/// functions are [`HostOrigin::Core`] and stay legal in a database-evaluated
/// position (a migration `$as`/`$back`, a view), unlike an application namespace
/// (§16.5). Every other resolved `$requires` contract is `Registered`.
const CORE_CODEC_CONTRACTS: &[&str] = &["liasse.base64", "liasse.hex", "liasse.string"];

/// The §16.5 origin of a `$requires`-resolved namespace: `Core` for the built-in
/// codec contracts the engine links, `Registered` for an application namespace.
fn origin_of(contract: &ContractRef) -> HostOrigin {
    if CORE_CODEC_CONTRACTS.contains(&contract.name().as_str()) {
        HostOrigin::Core
    } else {
        HostOrigin::Registered
    }
}

/// The resolved host components an activated package binds: the registered
/// [`Registry`] plus the resolved `$requires` map (§16.2). Owned by the engine.
pub(crate) struct HostBinding {
    registry: Registry,
    /// The local expression namespace → the semantic contract it pins.
    requires: BTreeMap<String, ContractRef>,
    /// The `$provider` names the application registered at initial load (§17.5),
    /// captured *before* any provider was `take`n out of the registry. It outlives
    /// the consumed provider boxes (a migration keeps this binding through
    /// [`rebind`](Self::rebind)), so a keyring reconstruction can tell a ring the
    /// application really backed — which must re-provision or refuse loudly, never
    /// silently sim — from a name it registered nothing for, which keeps the sim
    /// default (§17.5 honesty rule).
    registered_providers: BTreeSet<String>,
}

impl HostBinding {
    /// Resolve a package's `$requires` declarations against `registry`, ensuring
    /// the built-in cose contract is available (§16.2, §9.2 step 4).
    ///
    /// `strict` selects the failure discipline. When a caller supplies its host
    /// components ([`Engine::load_with_hosts`](crate::Engine::load_with_hosts),
    /// `strict = true`), §16.2 is enforced: a value that is not a `name@major`
    /// reference, or a requirement resolving to no / an incompatible / an
    /// ambiguous descriptor, fails before activation. When no host components are
    /// managed ([`Engine::load`](crate::Engine::load), `strict = false`), an
    /// unresolved requirement is *deferred* — omitted from the binding rather than
    /// failing the load — so a package whose host wiring lands later (the testkit's
    /// hosts-block driving) still loads; a call to a deferred namespace then fails
    /// as an unknown function rather than crashing.
    pub(crate) fn resolve(
        mut registry: Registry,
        requires: &[(String, String)],
        strict: bool,
    ) -> Result<Self, EngineError> {
        ensure_cose(&mut registry);
        // §17.5: record the registered provider names before any is consumed, so a
        // later reconstruction knows which rings the application really backed.
        let registered_providers = registry.provider_names().map(str::to_owned).collect();
        let requires = Self::bind(&registry, requires, strict)?;
        Ok(Self { registry, requires, registered_providers })
    }

    /// A binding that serves only the built-in core codec namespaces (§16.1):
    /// `base64`, `hex`, and the `string` byte codecs. The migration transform
    /// path (§20.1/§20.2) evaluates `base64.encode(string.bytes(.))` and its
    /// inverse in a scope with no package `$requires`, so it dispatches against
    /// this dedicated binding rather than the instance's own host components. The
    /// codec contracts are fixed literals, so `bind` cannot fail here.
    pub(crate) fn codecs() -> Self {
        let mut registry = Registry::new();
        for namespace in crate::codec::namespaces() {
            registry.register_namespace(namespace);
        }
        let requires = Self::bind(&registry, &crate::codec::requires(), false).unwrap_or_default();
        Self { registry, requires, registered_providers: BTreeSet::new() }
    }

    /// Re-resolve `requires` against the already-registered components (a §20
    /// migration keeps the context's registry but swaps the package's own
    /// requirement set).
    ///
    /// Strict, unlike the lenient default *genesis* load: an update targets a
    /// running instance whose host context is already settled, so a requirement
    /// that resolves to no descriptor cannot "land later" and must reject the
    /// update before activation (§2.1, §16.2 "missing requirements reject loading
    /// before the package becomes active"; §9.4/§E.9 leave the prior application
    /// active). A target that adds an unregistered `$requires` entry it never even
    /// calls therefore still fails the update rather than silently deferring.
    pub(crate) fn rebind(&mut self, requires: &[(String, String)]) -> Result<(), EngineError> {
        self.requires = Self::bind(&self.registry, requires, true)?;
        Ok(())
    }

    /// Bind each `(local, "name@major")` requirement to its resolved contract
    /// (§16.2). Under `strict`, an unparseable or unresolvable requirement is an
    /// [`EngineError::Requirement`]; otherwise it is deferred (skipped).
    fn bind(
        registry: &Registry,
        requires: &[(String, String)],
        strict: bool,
    ) -> Result<BTreeMap<String, ContractRef>, EngineError> {
        let mut resolved = BTreeMap::new();
        for (local, spec) in requires {
            let contract = match ContractRef::parse(spec) {
                Ok(contract) => contract,
                Err(error) if strict => {
                    return Err(EngineError::Requirement(format!(
                        "`$requires.{local}` = `{spec}`: {error}"
                    )));
                }
                Err(_) => continue,
            };
            match registry.resolve_namespace(&contract) {
                Ok(_) => {
                    resolved.insert(local.clone(), contract);
                }
                Err(error) if strict => {
                    return Err(EngineError::Requirement(format!("`$requires.{local}`: {error}")));
                }
                Err(_) => {}
            }
        }
        Ok(resolved)
    }

    /// The resolved namespaces' function signatures as expr-checker [`HostOp`]s
    /// (§16.2), keyed by local namespace then function — the descriptors a
    /// view/default/computed host call is type-checked against. The built-in cose
    /// namespace is excluded: its `sign`/`verify` are served through the managed
    /// keyring, not as generic value calls, so they are dispatched specially, not
    /// type-checked as host calls (see [`HostDispatch::eval_call`]).
    pub(crate) fn expr_signatures(&self) -> HostSignatures {
        let mut namespaces = BTreeMap::new();
        for (local, contract) in &self.requires {
            if contract.name().as_str() == COSE_CONTRACT {
                continue;
            }
            let Some(namespace) = self.namespace(local) else { continue };
            // §16.5: a resolved codec contract the engine links is Core (legal in a
            // database-evaluated position); every other application namespace is
            // Registered (legal only inside a mutation program).
            let origin = origin_of(contract);
            let mut functions = BTreeMap::new();
            for (name, func) in namespace.descriptor().functions() {
                let signature = func.signature();
                functions.insert(
                    name.clone(),
                    HostOp::new(
                        signature.params().iter().cloned(),
                        signature.result().clone(),
                        effect_of(func.effect()),
                        origin,
                    ),
                );
            }
            if !functions.is_empty() {
                namespaces.insert(local.clone(), functions);
            }
        }
        HostSignatures(namespaces)
    }

    /// The resolved contract a `$requires` local key names, if any.
    fn contract(&self, local: &str) -> Option<&ContractRef> {
        self.requires.get(local)
    }

    /// Whether `local` names a resolved host namespace (a `$requires` key).
    fn is_namespace(&self, local: &str) -> bool {
        self.requires.contains_key(local)
    }

    /// Whether `local` resolves the runtime's built-in cose contract (§17.7).
    fn is_cose(&self, local: &str) -> bool {
        self.contract(local).is_some_and(|c| c.name().as_str() == COSE_CONTRACT)
    }

    /// The registered namespace `local` names, resolved through the Annex E.8
    /// major-compatibility rule (`None` only if the load-time resolution was
    /// undone, which cannot happen for an activated package).
    fn namespace(&self, local: &str) -> Option<&dyn HostNamespace> {
        let contract = self.contract(local)?;
        self.registry.resolve_namespace(contract).ok()
    }

    /// Resolve a declared ring's `$provider` name to the provider that backs it
    /// (§17.5), moving it out of the registry, *together with* whether the
    /// application registered anything under that name at initial load.
    ///
    /// The two are read as a pair so a reconstruction can decide honestly when the
    /// provider is `None`: a name the application DID register but which is no
    /// longer available (consumed by an earlier ring, or gone across a
    /// reconstruction) must refuse loudly — never self-provision the forgeable sim
    /// double — while a name it never registered keeps the sim default (§17.5).
    pub(crate) fn resolve_provider(&mut self, name: &str) -> (Option<Box<dyn KeyProvider>>, bool) {
        let registered = self.registered_providers.contains(name);
        (self.registry.take_provider(name), registered)
    }
}

/// Seed a built-in cose namespace when the registry has none, so a package
/// requiring `liasse.cose@N` resolves against the runtime's native keyring-backed
/// implementation. A registry that already carries a cose descriptor is left
/// untouched (a host may register its own).
fn ensure_cose(registry: &mut Registry) {
    let Ok(cose_ref) = ContractRef::parse("liasse.cose@1") else { return };
    if matches!(registry.resolve_namespace(&cose_ref), Err(ResolutionError::Missing { .. })) {
        registry.register_namespace(Box::new(CoseNamespace::new()));
    }
}

/// The built-in `liasse.cose@1` descriptor holder. Its `sign`/`verify` are served
/// against the managed keyring (§17.7), not this value-only entry point, so its
/// [`HostNamespace::invoke`] reports the operation is served elsewhere; the
/// descriptor is what a `$requires: { cose }` resolves and pins.
struct CoseNamespace {
    descriptor: NamespaceDescriptor,
}

impl CoseNamespace {
    fn new() -> Self {
        Self { descriptor: cose_descriptor() }
    }
}

impl HostNamespace for CoseNamespace {
    fn descriptor(&self) -> &NamespaceDescriptor {
        &self.descriptor
    }

    fn invoke(&self, function: &str, _args: &[Value]) -> Result<Value, InvocationFailure> {
        Err(InvocationFailure::Unavailable {
            detail: format!(
                "cose.{function} is served through the managed keyring, not a value call"
            ),
        })
    }
}

/// The per-admission host-call dispatch the interpreter runs a `ns.fn(args)`
/// call against (§16.4, §17.7). Borrows the engine's [`HostBinding`] and live
/// keyrings; a `none` dispatch answers no namespace. It is a cheap borrow bundle
/// (a reference pair plus a clock), so it is `Copy` and an evaluation
/// [`Environment`](liasse_expr::Environment) can carry it for a view/default
/// host call as well as the interpreter.
#[derive(Clone, Copy)]
pub(crate) struct HostDispatch<'a> {
    binding: Option<&'a HostBinding>,
    keyrings: &'a [Keyring<EngineKeyProvider>],
    now: Timestamp,
}

impl<'a> HostDispatch<'a> {
    /// A dispatch with no host binding: a host-namespace call is not reachable
    /// (genesis seeding, view reads, migration transforms), so it answers no
    /// namespace and the interpreter type-checks the call as a language call.
    pub(crate) fn none(now: Timestamp) -> Self {
        Self { binding: None, keyrings: &[], now }
    }

    /// The live per-call dispatch a mutation admission runs against.
    pub(crate) fn new(
        binding: &'a HostBinding,
        keyrings: &'a [Keyring<EngineKeyProvider>],
        now: Timestamp,
    ) -> Self {
        Self { binding: Some(binding), keyrings, now }
    }

    /// Whether `local` names a resolved host namespace the interpreter dispatches
    /// rather than type-checks as a core-language call.
    pub(crate) fn is_namespace(&self, local: &str) -> bool {
        self.binding.is_some_and(|b| b.is_namespace(local))
    }

    /// Whether `local` names the built-in cose namespace (§17.7).
    pub(crate) fn is_cose(&self, local: &str) -> bool {
        self.binding.is_some_and(|b| b.is_cose(local))
    }

    /// The pinned result type of `local.function` (§16.2), so the interpreter
    /// binds a host result at its declared type.
    pub(crate) fn result_type(&self, local: &str, function: &str) -> Option<ExprType> {
        let namespace = self.binding?.namespace(local)?;
        let result = namespace.descriptor().function(function)?.signature().result().clone();
        Some(ExprType::scalar(result))
    }

    /// Invoke a generic host-namespace function through the [`ConformanceGuard`]
    /// (§16.2/§16.3): the returned value is validated against the declared result
    /// type, and a nonconforming return, a verifier rejection, or an unavailable
    /// dependency becomes a typed [`Rejection`] that commits no effect.
    pub(crate) fn invoke(
        &self,
        local: &str,
        function: &str,
        args: &[Value],
    ) -> Result<Value, Rejection> {
        // §16.1/§16.5: the core codec namespaces are engine-linked pure built-ins,
        // served statelessly ahead of any resolved `$requires` binding — a mutation
        // body may call `base64.encode`/`string.bytes` with no host component wired.
        if let Some(result) = crate::codec::invoke_core(local, function, args) {
            return result
                .map_err(|failure| host_rejection(local, function, &GuardError::Invocation(failure)));
        }
        let namespace = self.binding.and_then(|b| b.namespace(local)).ok_or_else(|| {
            Rejection::new(RejectionReason::Malformed, format!("unresolved host namespace `{local}`"))
        })?;
        // A fresh guard checks type conformance per call (§16.2 item 15). A memo
        // persisting a `pure` function's results across a whole run — the §16.3
        // replay-recompute soundness check — is a documented seam left to the
        // features layer that records generated results.
        let mut guard = ConformanceGuard::new();
        guard.invoke(namespace, function, args).map_err(|error| host_rejection(local, function, &error))
    }

    /// Invoke a host-namespace function for an *expression* position — a view, a
    /// field default, or a computed value (§16.2/§16.3) — returning a typed
    /// [`EvalError`] instead of a [`Rejection`], so a pure evaluation surfaces a
    /// nonconforming return, a verifier rejection, or an unavailable dependency
    /// through the ordinary evaluation-failure channel. The call runs through the
    /// same [`ConformanceGuard`] as the mutation path, so a component is never
    /// trusted to honour its declared contract. An environment with no live
    /// binding (genesis with no hosts, a migration transform) is a contract breach
    /// for a call the checker already resolved: [`EvalError::NoHostDispatch`].
    pub(crate) fn eval_call(
        &self,
        local: &str,
        function: &str,
        args: &[Value],
    ) -> Result<Value, EvalError> {
        // §16.1/§16.5: the core codec namespaces (`base64`/`hex`/`string` byte
        // codecs) are engine-linked pure built-ins available in EVERY
        // database-evaluated position — a view, a `$check`, a computed value, a
        // field default, a §20 `$as`/`$back` transform — independent of the
        // package's resolved `$requires`. Serve them statelessly here, ahead of the
        // per-instance binding, so a position carrying a [`HostDispatch::none`]
        // (a bare view read, a bucket bound) resolves them too.
        if let Some(result) = crate::codec::invoke_core(local, function, args) {
            return result.map_err(|failure| EvalError::HostCall {
                detail: host_detail(local, function, &GuardError::Invocation(failure)),
            });
        }
        let namespace = self
            .binding
            .and_then(|b| b.namespace(local))
            .ok_or(EvalError::NoHostDispatch)?;
        let mut guard = ConformanceGuard::new();
        guard.invoke(namespace, function, args).map_err(|error| EvalError::HostCall {
            detail: host_detail(local, function, &error),
        })
    }

    /// Sign `claims` through keyring `ring`'s active version (§17.7/§17.8),
    /// returning the token value. A provider outage rejects (§17.9) and mints no
    /// token. The token carries the provider's GENUINE signature over the claims'
    /// canonical bytes (never the plaintext claims), so it verifies only under the
    /// active version's public key and cannot be reconstructed from public metadata.
    pub(crate) fn cose_sign(&self, ring: &str, claims_value: &Value) -> Result<Value, Rejection> {
        let claims = CoseClaims::from_value(claims_value).ok_or_else(|| {
            Rejection::new(RejectionReason::TypeError, "`cose.sign` claims must be an object")
        })?;
        let keyring = self.keyring(ring)?;
        let signed = claims.signing_bytes();
        let token = keyring
            .sign(&signed, self.now)
            .map_err(|error| Rejection::new(RejectionReason::Host, format!("cose.sign failed: {error}")))?;
        Ok(CoseToken::new(ring, token.version().get(), claims, token.signature().to_vec()).to_value())
    }

    fn keyring(&self, ring: &str) -> Result<&Keyring<EngineKeyProvider>, Rejection> {
        self.keyrings.iter().find(|r| r.name() == ring).ok_or_else(|| {
            Rejection::new(RejectionReason::Malformed, format!("`cose.sign` names unknown keyring `/{ring}`"))
        })
    }
}

/// Map a guarded-invocation failure to an admission [`Rejection`] (§16.3): a
/// verifier rejection, an unavailable dependency, and a conformance violation are
/// each a host refusal that commits no effect.
fn host_rejection(local: &str, function: &str, error: &GuardError) -> Rejection {
    Rejection::new(RejectionReason::Host, host_detail(local, function, error))
}

/// The sanitized diagnostic detail of a guarded host-call failure (§16.3, §23.8),
/// shared by the mutation-path [`Rejection`] and the expression-path
/// [`EvalError`](liasse_expr::EvalError).
fn host_detail(local: &str, function: &str, error: &GuardError) -> String {
    match error {
        GuardError::Invocation(InvocationFailure::Verification { detail }) => {
            format!("`{local}.{function}` verification failed: {detail}")
        }
        GuardError::Invocation(failure) => format!("`{local}.{function}`: {failure}"),
        GuardError::Violation(violation) => {
            format!("`{local}.{function}` returned a nonconforming value: {violation}")
        }
    }
}

/// Why a [`cose_verify`](crate::Engine::cose_verify) refused a token (§17.7).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoseVerifyError {
    /// The value is not a well-formed cose token.
    #[error("not a well-formed cose token")]
    Malformed,
    /// No keyring is provisioned under the named ring.
    #[error("no keyring named `{0}` is provisioned")]
    UnknownRing(String),
    /// The token names a different keyring than the one verifying it (§17.7).
    #[error("token was signed by a different keyring")]
    WrongRing,
    /// The token's signature does not verify over its claims under the accepted
    /// version's public key — a forged token, or a claim set altered after signing.
    #[error("token claims do not match the signed payload")]
    ClaimsTampered,
    /// The token's version is not currently accepted — retired past `$retain`,
    /// revoked, or destroyed (§17.7).
    #[error("token version is no longer accepted")]
    VersionNotAccepted,
    /// The accepted version names an algorithm the cose codec cannot verify
    /// (§17.8): reject loudly rather than fall back to any weaker check.
    #[error("token signature algorithm is unsupported: {0}")]
    UnsupportedAlgorithm(String),
}

impl From<SignatureError> for CoseVerifyError {
    /// Map a §17.8 codec signature failure to a token rejection. An unsupported
    /// algorithm stays a loud, distinct failure; a verification failure or a
    /// malformed accepted key is a claims/signature mismatch (fail-closed).
    fn from(error: SignatureError) -> Self {
        match error {
            SignatureError::UnsupportedAlgorithm(algorithm) => {
                Self::UnsupportedAlgorithm(algorithm)
            }
            SignatureError::Invalid | SignatureError::MalformedPublicKey(_) => Self::ClaimsTampered,
        }
    }
}

/// Verify `token_value` against keyring `ring`'s accepted versions at `now`
/// (§17.7): the token's ring must match, its version must be currently accepted,
/// and its signature must cryptographically verify over the claims' canonical
/// bytes under that accepted version's PUBLIC key. Verification consults only
/// public material — no provider operation is involved — so an existing token
/// keeps verifying through a provider outage, while a revoked / retired-past-
/// `$retain` / foreign-ring / forged / tampered token is denied. Returns the
/// verified claims together with the accepted key-version identity (§17.7: "the
/// result includes the verified key-version identity") so authentication policy
/// can reject an accepted-but-disallowed version. Used by the surface/testkit
/// auth path.
pub(crate) fn cose_verify(
    keyrings: &[Keyring<EngineKeyProvider>],
    ring: &str,
    token_value: &Value,
    now: Timestamp,
) -> Result<(Value, VersionId), CoseVerifyError> {
    let token = CoseToken::from_value(token_value).ok_or(CoseVerifyError::Malformed)?;
    let keyring = keyrings
        .iter()
        .find(|r| r.name() == ring)
        .ok_or_else(|| CoseVerifyError::UnknownRing(ring.to_owned()))?;
    if token.ring() != ring {
        return Err(CoseVerifyError::WrongRing);
    }
    // §17.7: verification uses the accepted PUBLIC versions. A revoked / retired-
    // past-`$retain` / never-accepted version is absent here, so there is no
    // public key to verify against — the token is denied before any crypto.
    let version = keyring
        .accepted(now)
        .into_iter()
        .find(|version| version.id().get() == token.version())
        .ok_or(CoseVerifyError::VersionNotAccepted)?;
    // §17.7/§17.8: cryptographically verify the signature under the accepted
    // version's public key. A signature-blind check would let a token forged from
    // public metadata (ring + accepted ordinal + plaintext claim bytes) pass.
    verify_cose_signature(
        version.algorithm(),
        version.public_key(),
        &token.claims().signing_bytes(),
        token.signature(),
    )?;
    Ok((token.claims().as_struct(), version.id()))
}
