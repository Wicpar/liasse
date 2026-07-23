//! Executable simulated host namespaces for the §11.5 auth-mutation pattern.
//!
//! A conformance case's `$requires` may name a simulated verifier/token namespace
//! declared only by a behaviour table — `webauthn: { $namespace, responses }`,
//! `token: { $namespace }` — with no typed `functions` roster (the descriptor form
//! adapter/namespaces.rs builds). §16.5 makes such a namespace callable only inside
//! a mutation program: `identity = webauthn.verify(@response)`,
//! `token = token.sign({ auth, session })`. This module turns a namespace that a
//! mutation body actually CALLS into an executable [`HostNamespace`] whose behaviour
//! is the case's declared table, so [`Engine::load_with_dispatch`] dispatches those
//! mutation-body calls and produces the recorded results (§8.12) the login returns.
//!
//! The minted token is *self-describing*: `token.sign(claims)` encodes the claims
//! struct into an opaque text, and the auth layer's `token.verify` (adapter/auth.rs)
//! decodes the SAME text back to typed claims (§11.3) — no shared mutable
//! minted-token registry, so the mutation namespace and the authenticator agree by
//! construction and every login is deterministic.
//!
//! Only namespaces a `$mut` body invokes are synthesized here; a namespace used
//! solely in a database-evaluated `$verify` (the §11 static-token cases) carries no
//! mutation-body call and stays on the lenient default load, its credential→proof
//! table reconstructed at the auth layer exactly as before.

use std::collections::BTreeMap;

use liasse_host::{
    ContractRef, EffectClass, FunctionDescriptor, HostNamespace, InterfaceHash, InvocationFailure,
    NamespaceDescriptor, OpSignature, Registry, Version,
};
use liasse_value::{Struct, StructType, Text, Type, Value};
use serde_json::Value as J;

use crate::hosts::{HostKind, HostsConfig};

/// The self-describing minted-token marker. `token.sign(claims)` emits
/// `PREFIX + canonical_json(claims)`; [`decode_token`] recovers the claims object.
const TOKEN_PREFIX: &str = "liasse-authsim:v1:";

/// Encode a `token.sign` claims value into a self-describing token text (§8.12):
/// the marker followed by the claims' canonical JSON. Deterministic in the claims,
/// so two logins with distinct session keys mint distinct tokens while a replay of
/// one login's claims reproduces its token exactly.
#[must_use]
pub(super) fn sign_token(claims: &Value) -> Text {
    Text::new(format!("{TOKEN_PREFIX}{}", claims.to_canonical_json_string()))
}

/// Decode a self-describing minted token back to its claims object (§11.3), or
/// `None` when `text` is not one this harness minted. The auth layer reads the
/// `auth`/`session`/`account` members from the returned object.
#[must_use]
pub(super) fn decode_token(text: &str) -> Option<serde_json::Map<String, J>> {
    let body = text.strip_prefix(TOKEN_PREFIX)?;
    match serde_json::from_str::<J>(body).ok()? {
        J::Object(map) => Some(map),
        _ => None,
    }
}

/// One synthesized function's behaviour.
enum FnBehavior {
    /// `verify(text) -> struct`: look the credential up in the declared table and
    /// return the mapped proof struct (§16.3 verifier). An unknown credential is a
    /// verification diagnostic — authentication fails inside the mutation (§11.5).
    Lookup(BTreeMap<String, Value>),
    /// `sign(claims) -> text`: mint a self-describing token from the claims struct
    /// (§16.3 generated, §17.8 native token construction stand-in).
    SignToken,
}

/// An executable simulated namespace whose functions come from a case's behaviour
/// tables, dispatched inside a mutation body (§16.5).
pub(super) struct AuthSimNamespace {
    descriptor: NamespaceDescriptor,
    functions: BTreeMap<String, FnBehavior>,
}

impl HostNamespace for AuthSimNamespace {
    fn descriptor(&self) -> &NamespaceDescriptor {
        &self.descriptor
    }

    fn invoke(&self, function: &str, args: &[Value]) -> Result<Value, InvocationFailure> {
        let behavior = self
            .functions
            .get(function)
            .ok_or_else(|| InvocationFailure::UnknownFunction(function.to_owned()))?;
        match behavior {
            FnBehavior::Lookup(table) => lookup(function, table, args),
            FnBehavior::SignToken => Ok(Value::Text(sign_token(single_arg(function, args)?))),
        }
    }
}

/// A verifier lookup: the single text credential selects its proof, or fails
/// verification (§16.3). A non-text argument is malformed input.
fn lookup(
    function: &str,
    table: &BTreeMap<String, Value>,
    args: &[Value],
) -> Result<Value, InvocationFailure> {
    let Some(credential) = credential_text(single_arg(function, args)?) else {
        return Err(InvocationFailure::Verification {
            detail: format!("`{function}` credential is not a text token"),
        });
    };
    table.get(credential).cloned().ok_or_else(|| InvocationFailure::Verification {
        detail: format!("`{function}` credential is not accepted"),
    })
}

/// The text of a credential argument. A mutation body compiled under the lenient
/// dispatch load leaves the parameter untyped, so `@response`/`@credential` reaches
/// the namespace as `Value::Json` string rather than `Value::Text`; both carry the
/// same token text, so either is accepted.
fn credential_text(value: &Value) -> Option<&str> {
    match value {
        Value::Text(text) => Some(text.as_str()),
        Value::Json(liasse_value::Json::String(text)) => Some(text.as_str()),
        _ => None,
    }
}

/// The sole argument of a unary function, or an arity failure.
fn single_arg<'a>(function: &str, args: &'a [Value]) -> Result<&'a Value, InvocationFailure> {
    match args {
        [arg] => Ok(arg),
        _ => Err(InvocationFailure::Arity { function: function.to_owned(), expected: 1, found: args.len() }),
    }
}

/// The synthesized executable namespaces a case's mutation bodies call (§16.5).
/// Empty when no `$mut` statement invokes a `$requires`-registered namespace, so
/// only cases exercising the §11.5 auth-mutation pattern take the dispatch path.
#[must_use]
pub(super) fn synthesize(package: &J, hosts: Option<&J>) -> Vec<AuthSimNamespace> {
    let requires = requires_map(package);
    if requires.is_empty() {
        return Vec::new();
    }
    let mut statements = Vec::new();
    collect_mut_statements(package.get("$model"), &mut statements);
    // local namespace -> the function names its mutation bodies call.
    let mut called: BTreeMap<&str, std::collections::BTreeSet<String>> = BTreeMap::new();
    for statement in &statements {
        for (local, function) in namespace_calls(statement, &requires) {
            called.entry(local).or_default().insert(function);
        }
    }
    let components = hosts.map(HostsConfig::parse).unwrap_or_default();
    called
        .into_iter()
        .filter(|(local, _)| !is_descriptor_form(local, &components))
        .filter_map(|(local, functions)| {
            let contract = requires.get(local)?;
            build_namespace(local, contract, &functions, &components)
        })
        .collect()
}

/// Register `namespaces` (plus any descriptor-form `extra`) into a fresh registry
/// for [`Engine::load_with_dispatch`].
#[must_use]
pub(super) fn registry(
    namespaces: Vec<AuthSimNamespace>,
    extra: Vec<liasse_host::sim::SimNamespace>,
) -> Registry {
    let mut registry = Registry::new();
    for namespace in namespaces {
        registry.register_namespace(Box::new(namespace));
    }
    for namespace in extra {
        registry.register_namespace(Box::new(namespace));
    }
    registry
}

/// Whether any `$requires` namespace is called inside a `$mut` body — the trigger
/// for the §16.5 dispatch load path.
#[must_use]
pub(super) fn has_mutation_host_call(package: &J) -> bool {
    let requires = requires_map(package);
    if requires.is_empty() {
        return false;
    }
    let mut statements = Vec::new();
    collect_mut_statements(package.get("$model"), &mut statements);
    statements.iter().any(|statement| namespace_calls(statement, &requires).next().is_some())
}

/// Build one executable namespace for `local`, resolving its contract identity from
/// the `$requires` value and its lookup table from the matching `hosts` component.
fn build_namespace(
    local: &str,
    contract: &str,
    functions: &std::collections::BTreeSet<String>,
    components: &HostsConfig,
) -> Option<AuthSimNamespace> {
    let reference = ContractRef::parse(contract).ok()?;
    let name = reference.name().clone();
    let version = Version { major: reference.major(), minor: 0, patch: 0 };
    let interface_hash = InterfaceHash::new(contract);
    let table = lookup_table(local, contract, components);

    let mut descriptors = Vec::new();
    let mut behaviors = BTreeMap::new();
    for function in functions {
        let (signature, effect, behavior) = match function.as_str() {
            "sign" => (
                OpSignature::new([Type::Json], Type::Text),
                EffectClass::Generated,
                FnBehavior::SignToken,
            ),
            // A verifier: the declared table maps each credential to its proof
            // struct; its result type is that struct's shape (§16.2/§16.3).
            _ => {
                let result = table.values().next().map_or(Type::Json, value_type);
                (
                    OpSignature::new([Type::Text], result),
                    EffectClass::Verifier,
                    FnBehavior::Lookup(table.clone()),
                )
            }
        };
        descriptors.push((function.clone(), FunctionDescriptor::new(signature, effect)));
        behaviors.insert(function.clone(), behavior);
    }
    let descriptor = NamespaceDescriptor::new(name, version, interface_hash, [], descriptors);
    Some(AuthSimNamespace { descriptor, functions: behaviors })
}

/// The credential→proof table the `hosts` component for `local` declares, decoded
/// to typed struct [`Value`]s. Read from `responses` (webauthn) or `accepts`; empty
/// for a token component that carries none.
fn lookup_table(local: &str, contract: &str, components: &HostsConfig) -> BTreeMap<String, Value> {
    let mut table = BTreeMap::new();
    let component = components.components.iter().find(|component| {
        component.kind == HostKind::Namespace
            && (component.label == local
                || component.config.get("$namespace").and_then(J::as_str) == Some(contract))
    });
    let Some(component) = component else { return table };
    for key in ["responses", "accepts"] {
        if let Some(entries) = component.config.get(key).and_then(J::as_object) {
            for (credential, proof) in entries {
                if let Some(value) = json_struct(proof) {
                    table.insert(credential.clone(), value);
                }
            }
        }
    }
    table
}

/// Whether `local`'s `hosts` component is the *descriptor* form (a typed
/// `functions` roster, adapter/namespaces.rs builds it as a `SimNamespace`): such a
/// namespace is registered from its descriptor and must not be synthesized here too,
/// or the contract would resolve ambiguously.
fn is_descriptor_form(local: &str, components: &HostsConfig) -> bool {
    components.components.iter().any(|component| {
        component.kind == HostKind::Namespace
            && component.label == local
            && component.config.get("functions").and_then(J::as_object).is_some_and(|f| !f.is_empty())
    })
}

/// A JSON object → a typed struct value with text/bool members (the shape a
/// simulated verifier proof carries). `None` for a non-object proof.
fn json_struct(proof: &J) -> Option<Value> {
    let object = proof.as_object()?;
    let fields = object.iter().map(|(name, value)| (Text::new(name), json_scalar(value)));
    Some(Value::Struct(Struct::new(fields)))
}

/// One JSON member as a scalar value: a string is text, a bool is bool; any other
/// form falls back to its textual rendering so the proof stays well-formed.
fn json_scalar(value: &J) -> Value {
    match value {
        J::String(text) => Value::Text(Text::new(text)),
        J::Bool(flag) => Value::Bool(*flag),
        other => Value::Text(Text::new(other.to_string())),
    }
}

/// The declared type of a synthesized proof value, so the pinned result type
/// matches the value the verifier returns (the [`liasse_host::ConformanceGuard`]
/// checks the return against it, §16.2).
fn value_type(value: &Value) -> Type {
    match value {
        Value::Struct(fields) => Type::Struct(StructType::new(
            fields.fields().map(|(name, member)| (name.as_str().to_owned(), value_type(member))),
        )),
        Value::Bool(_) => Type::Bool,
        _ => Type::Text,
    }
}

/// The package's `$requires` as a local → contract-string map.
fn requires_map(package: &J) -> BTreeMap<String, String> {
    package
        .get("$requires")
        .and_then(J::as_object)
        .map(|requires| {
            requires
                .iter()
                .filter_map(|(local, spec)| spec.as_str().map(|spec| (local.clone(), spec.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

/// Every `$mut` statement string in the model, walking top-level and nested
/// collection `$mut` blocks. A statement is a string; a mutation is a sequence of
/// them (§8), or occasionally a single string.
fn collect_mut_statements(value: Option<&J>, out: &mut Vec<String>) {
    let Some(value) = value else { return };
    match value {
        J::Object(map) => {
            for (key, child) in map {
                if key == "$mut"
                    && let Some(mutations) = child.as_object()
                {
                    for body in mutations.values() {
                        push_statements(body, out);
                    }
                }
                collect_mut_statements(Some(child), out);
            }
        }
        J::Array(items) => {
            for item in items {
                collect_mut_statements(Some(item), out);
            }
        }
        _ => {}
    }
}

/// A mutation body's statement strings (an array of strings, or a single string).
fn push_statements(body: &J, out: &mut Vec<String>) {
    match body {
        J::String(statement) => out.push(statement.clone()),
        J::Array(items) => out.extend(items.iter().filter_map(J::as_str).map(ToOwned::to_owned)),
        _ => {}
    }
}

/// The `local.function(` namespace calls in one statement whose `local` names a
/// `$requires` entry. A bare identifier boundary before `local` avoids matching a
/// field-access `row.webauthn` that merely shares the name.
fn namespace_calls<'a>(
    statement: &'a str,
    requires: &'a BTreeMap<String, String>,
) -> impl Iterator<Item = (&'a str, String)> {
    requires.keys().filter_map(move |local| {
        let function = call_function(statement, local)?;
        Some((local.as_str(), function))
    })
}

/// If `statement` calls `local.<function>(`, the function name. The match requires
/// `local` to sit on an identifier boundary (not the tail of `row.local`) and be
/// followed by `.<ident>(`.
fn call_function(statement: &str, local: &str) -> Option<String> {
    let bytes = statement.as_bytes();
    let mut from = 0;
    while let Some(offset) = statement[from..].find(local) {
        let start = from + offset;
        let end = start + local.len();
        let before_ok = !bytes.get(start.wrapping_sub(1)).is_some_and(|byte| is_ident_byte(*byte));
        if before_ok && statement[end..].starts_with('.') {
            let rest = &statement[end + 1..];
            let name_len = rest.find(|c: char| !is_ident_char(c)).unwrap_or(rest.len());
            if name_len > 0 && rest[name_len..].starts_with('(') {
                return Some(rest[..name_len].to_owned());
            }
        }
        from = end;
    }
    None
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}
