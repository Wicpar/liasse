//! Building a host [`Registry`] from a case's `hosts.namespaces` block (§16).
//!
//! §16 cases declare simulated namespace descriptors — a contract id, version,
//! interface hash, and a typed function roster with an effect class and a
//! deterministic `op` behaviour (tests/16-host-namespaces/NOTES.md). This module
//! parses that block into a [`SimNamespace`] per descriptor and registers them,
//! so [`Engine::load_with_hosts`](liasse_runtime::Engine::load_with_hosts)
//! resolves the package's `$requires` strictly (§16.2) and a host-namespace call
//! in a view/default/verifier type-checks and evaluates against the pinned
//! descriptor.
//!
//! Only the *descriptor* form — a component carrying a `functions` roster — is
//! built here. A grouped `namespaces: { cose: "liasse.cose@1" }` entry (a bare
//! contract string, no roster) and the §11 verifier-table form
//! (`{ $namespace, tokens }`, no roster) carry no descriptor to register: the
//! built-in cose namespace is seeded by the runtime, and the §11 auth verifier is
//! reconstructed at the auth layer (adapter/auth.rs).

use liasse_host::sim::{Behavior, SimNamespace};
use liasse_host::{
    ContractName, EffectClass, InterfaceHash, NamespaceType, OpSignature, Registry, Version,
};
use liasse_value::{StructType, Type};
use serde_json::Value as J;

use crate::hosts::{HostKind, HostsConfig};

/// The simulated namespace descriptors a case's `hosts` block declares — every
/// component whose config carries a typed `functions` roster (§16.2). A block
/// with no such descriptor (grouped contract strings, §11 verifier tables) yields
/// an empty list.
#[must_use]
pub(super) fn sim_namespaces(hosts: Option<&J>) -> Vec<SimNamespace> {
    let Some(hosts) = hosts else { return Vec::new() };
    HostsConfig::parse(hosts)
        .components
        .iter()
        .filter(|component| component.kind == HostKind::Namespace)
        .filter_map(|component| build_namespace(&component.label, &component.config))
        .collect()
}

/// Register `namespaces` into a fresh registry, so the engine resolves `$requires`
/// against them (§16.2).
#[must_use]
pub(super) fn registry(namespaces: Vec<SimNamespace>) -> Registry {
    let mut registry = Registry::new();
    for namespace in namespaces {
        registry.register_namespace(Box::new(namespace));
    }
    registry
}

/// Build one [`SimNamespace`] from a descriptor component. `None` when the config
/// carries no function roster (there is no descriptor to register) or its id /
/// version does not parse as the §16.2 contract identity.
fn build_namespace(label: &str, config: &J) -> Option<SimNamespace> {
    let functions = config.get("functions").and_then(J::as_object)?;
    if functions.is_empty() {
        return None;
    }
    let id = config.get("id").and_then(J::as_str).unwrap_or(label);
    let name = ContractName::parse(id).ok()?;
    let version = Version::parse(config.get("version").and_then(J::as_str)?).ok()?;
    let interface_hash = InterfaceHash::new(config.get("interface_hash").and_then(J::as_str).unwrap_or(id));

    let mut builder = SimNamespace::builder(name, version, interface_hash);
    for (function, spec) in functions {
        let Some((signature, effect, behavior)) = read_function(spec) else { continue };
        builder = builder.function(function.clone(), signature, effect, behavior);
    }
    if let Some(types) = config.get("types").and_then(J::as_object) {
        for (name, spec) in types {
            if let Some(ty) = read_named_type(spec) {
                builder = builder.named_type(name.clone(), ty);
            }
        }
    }
    Some(builder.build())
}

/// Parse one function's `signature`/`effect`/`op` into its typed descriptor and
/// deterministic behaviour. The `accepts` table a verifier declares is not needed
/// on the descriptor — the §16 verifier that reads it runs at the auth layer, not
/// through this registered namespace — so it is not carried here.
fn read_function(spec: &J) -> Option<(OpSignature, EffectClass, Behavior)> {
    let signature = parse_signature(spec.get("signature").and_then(J::as_str)?)?;
    let effect = parse_effect(spec.get("effect").and_then(J::as_str)?)?;
    let behavior = parse_behavior(spec.get("op").and_then(J::as_str)?)?;
    Some((signature, effect, behavior))
}

/// A namespace-defined named value type (`{ codec, key_eligible }`, §16.4).
fn read_named_type(spec: &J) -> Option<NamespaceType> {
    let codec = spec.get("codec").and_then(J::as_str)?;
    let key_eligible = spec.get("key_eligible").and_then(J::as_bool).unwrap_or(false);
    Some(NamespaceType::new(codec, key_eligible))
}

/// Parse a `(a, b) -> r` signature string into a typed [`OpSignature`].
fn parse_signature(text: &str) -> Option<OpSignature> {
    let (params, result) = text.split_once("->")?;
    let params = params.trim();
    let params = params.strip_prefix('(')?.strip_suffix(')')?.trim();
    let param_types = if params.is_empty() {
        Vec::new()
    } else {
        params.split(',').map(parse_type).collect()
    };
    Some(OpSignature::new(param_types, parse_type(result)))
}

/// Parse a type spelling into a [`Type`]: a scalar token, or a `{ f: t, ... }`
/// struct. An unrecognized token falls back to `text`, which is enough for
/// descriptor registration (the interface hash, pinned explicitly by the case,
/// carries the semantic identity).
fn parse_type(text: &str) -> Type {
    let text = text.trim();
    if let Some(inner) = text.strip_prefix('{').and_then(|rest| rest.strip_suffix('}')) {
        let fields = inner
            .split(',')
            .filter_map(|field| field.split_once(':'))
            .map(|(name, ty)| (name.trim().to_owned(), parse_type(ty)));
        return Type::Struct(StructType::new(fields));
    }
    match text {
        "int" => Type::Int,
        "bool" => Type::Bool,
        "bytes" => Type::Bytes,
        "uuid" => Type::Uuid,
        "decimal" => Type::Decimal,
        "date" => Type::Date,
        "duration" => Type::Duration,
        "json" => Type::Json,
        _ => Type::Text,
    }
}

/// The §16.3 effect class a function declares.
fn parse_effect(text: &str) -> Option<EffectClass> {
    Some(match text {
        "pure" => EffectClass::Pure,
        "verifier" => EffectClass::Verifier,
        "generated" => EffectClass::Generated,
        _ => return None,
    })
}

/// The deterministic behaviour a function's `op` names (§16 NOTES.md).
fn parse_behavior(text: &str) -> Option<Behavior> {
    Some(match text {
        "double" => Behavior::Double,
        "token" => Behavior::Token,
        "accept" => Behavior::Accept,
        "off_type" => Behavior::OffType,
        "drifting" => Behavior::Drifting,
        _ => return None,
    })
}
