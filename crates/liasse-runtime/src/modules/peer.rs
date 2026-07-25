//! §13.5 peer-dependency resolution against the sibling instance set.
//!
//! A peer `$use` handle (`people: "acme.people/people@1"`) binds to a **sibling**
//! module instance in the *same* module collection (§13.5): the part before `/`
//! names the package line, the part after names the exposed interface, and `@N`
//! selects a compatible major (§13.14 — breaking changes use a new major, so a
//! candidate is compatible with `@N` only when its major equals `N`). Resolution
//! considers compatible siblings in exactly the same collection:
//!
//! ```text
//! one candidate      bind automatically
//! several candidates require an explicit operator binding (§13.3 `$use`)
//! zero candidates    reject a required binding (an optional one is absent)
//! ```
//!
//! A disabled sibling exposes no peer availability (§13.12), so it is not a
//! candidate. An explicit `$use` binding names a sibling by INSTANCE NAME
//! (`people2`); since the sibling set is exactly the module collection being
//! installed into, a name is the whole coordinate and there is no cross-collection
//! spelling to get wrong. A compatible instance in another collection is not a
//! candidate.

use crate::modules::install::{AdmittedBindings, UseSpec};
use crate::modules::ModuleError;

/// One enabled sibling instance in a module collection, reduced to what peer resolution
/// needs: its local name, its package compatibility line and major (§13.5/§13.14),
/// and the interfaces it exposes readably (§13.8). A disabled sibling is omitted by
/// the caller, so every entry is an available candidate.
pub(crate) struct SiblingInterface {
    pub(crate) name: String,
    pub(crate) line: String,
    pub(crate) major: u64,
    pub(crate) interfaces: Vec<String>,
}

impl SiblingInterface {
    /// Whether this sibling is a compatible candidate for a peer requirement of
    /// `line@major` exposing `interface` (§13.5): the package line matches, the
    /// major matches exactly (§13.14 breaking-change discipline), and it exposes the
    /// named interface.
    fn satisfies(&self, line: &str, interface: &str, major: u64) -> bool {
        self.line == line && self.major == major && self.interfaces.iter().any(|i| i == interface)
    }
}

/// A peer handle resolved against the sibling set (§13.5), recording the concrete
/// sibling instance the handle binds to (`§13.3 `$resolved`) or its absence
/// (`$absent`). An optional handle with no candidate resolves to
/// [`instance`](Self::instance) `None` and is bound as absent (§13.5 `$optional`).
pub(crate) struct ResolvedPeer {
    /// The child-visible handle name (`people`).
    pub(crate) handle: String,
    /// The exposed interface the handle reads through (`people`).
    pub(crate) interface: String,
    /// The resolved sibling instance name in the same collection, or `None` for an
    /// absent optional peer.
    pub(crate) instance: Option<String>,
    /// Whether the handle is an optional peer (§13.5 `$optional`): its absence is
    /// valid and it binds a presence value rather than a required interface view.
    pub(crate) optional: bool,
}

/// Resolve every peer `$use` requirement of a child being installed against the
/// enabled `siblings` already present in its module collection (§13.5). A required
/// requirement with zero, several, or only incompatible candidates — or an explicit
/// binding naming a non-sibling — is rejected as [`ModuleError::PeerUnresolved`]; an
/// optional requirement with no candidate resolves as absent. A non-peer handle
/// (`$parent`, a private `$deps`) contributes no resolution here.
pub(crate) fn resolve(
    bindings: &AdmittedBindings,
    siblings: &[SiblingInterface],
) -> Result<Vec<ResolvedPeer>, ModuleError> {
    let mut resolved = Vec::new();
    for (handle, spec, optional) in &bindings.uses {
        let UseSpec::Peer { line, interface, major } = spec else {
            continue;
        };
        // §13.3: when peer resolution finds several candidates the operator supplies
        // an explicit `$use` path binding for the handle; it overrides auto-resolution.
        let instance = match explicit_binding(bindings, handle) {
            Some(name) => Some(resolve_explicit(handle, name, line, interface, *major, siblings)?),
            None => resolve_auto(handle, line, interface, *major, *optional, siblings)?,
        };
        resolved.push(ResolvedPeer {
            handle: handle.clone(),
            interface: interface.clone(),
            instance,
            optional: *optional,
        });
    }
    Ok(resolved)
}

/// The explicit sibling-path binding an operator supplied for `handle` under the
/// install request's `$use` (§13.3), if any: a [`UseSpec::Sibling`] recorded for
/// the same handle name as the peer requirement.
fn explicit_binding<'a>(bindings: &'a AdmittedBindings, handle: &str) -> Option<&'a str> {
    bindings.uses.iter().find_map(|(name, spec, _)| match spec {
        UseSpec::Sibling(sibling) if name == handle => Some(sibling.as_str()),
        _ => None,
    })
}

/// Resolve an explicit `$use` sibling binding (§13.3): the named instance must be
/// one of `siblings` — the enabled instances of the very collection being installed
/// into — and a compatible candidate for the requirement. The candidate set IS the
/// collection, so a name that is not in it resolves to nothing rather than reaching
/// out of the sibling set.
fn resolve_explicit(
    handle: &str,
    name: &str,
    line: &str,
    interface: &str,
    major: u64,
    siblings: &[SiblingInterface],
) -> Result<String, ModuleError> {
    match siblings.iter().find(|s| s.name == name) {
        Some(sibling) if sibling.satisfies(line, interface, major) => Ok(name.to_owned()),
        Some(_) => Err(ModuleError::PeerUnresolved(
            handle.to_owned(),
            format!("sibling `{name}` does not expose interface `{interface}` at major {major} on line `{line}`"),
        )),
        None => Err(ModuleError::PeerUnresolved(
            handle.to_owned(),
            format!("explicit binding names `{name}`, which is not an installed sibling in this module collection"),
        )),
    }
}

/// Resolve a peer requirement with no explicit binding against the candidate set
/// (§13.5 resolution table): exactly one candidate auto-binds; several require an
/// explicit binding (rejected); zero rejects a required requirement, or resolves an
/// optional one as absent.
fn resolve_auto(
    handle: &str,
    line: &str,
    interface: &str,
    major: u64,
    optional: bool,
    siblings: &[SiblingInterface],
) -> Result<Option<String>, ModuleError> {
    let mut candidates = siblings.iter().filter(|s| s.satisfies(line, interface, major));
    let Some(first) = candidates.next() else {
        return if optional {
            Ok(None)
        } else {
            Err(ModuleError::PeerUnresolved(
                handle.to_owned(),
                format!("no enabled sibling in the module collection exposes interface `{interface}` at major {major} on line `{line}`"),
            ))
        };
    };
    if candidates.next().is_some() {
        return Err(ModuleError::PeerUnresolved(
            handle.to_owned(),
            "several compatible candidates require an explicit `$use` binding naming one sibling".to_owned(),
        ));
    }
    Ok(Some(first.name.clone()))
}
