//! Typed parse of a case's `hosts` block into host-component configuration.
//!
//! FORMAT.md's `hosts` block provisions the simulated host components a case
//! runs against (§16 namespaces, §17 key providers, §18 blob connectors). The
//! corpus writes it two ways: a *grouped* form under `namespaces` /
//! `key_providers` / `connectors`, and a *named-component* form where each
//! top-level key is a component whose kind is implied by its members. This
//! module normalizes both into a flat list of typed [`HostComponent`]s — the
//! configuration data (which components exist, their kind, their label) is
//! typed; each component's fine behaviour stays verbatim for the future
//! crate-internal driver adapter to translate into `liasse-host` doubles. It
//! produces *config*, never live objects.

use serde_json::Value;

use crate::case::Case;

/// The host-component kinds a `hosts` block can provision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKind {
    /// A §16 host namespace (typed operations, `functions`, tokens).
    Namespace,
    /// A §17 key provider (algorithms, operations, protection class).
    KeyProvider,
    /// A §18 blob connector (declared capabilities).
    Connector,
    /// A component whose kind the block does not make explicit.
    Other,
}

impl HostKind {
    fn of_group(bucket: &str) -> Option<Self> {
        match bucket {
            "namespaces" => Some(Self::Namespace),
            "key_providers" => Some(Self::KeyProvider),
            "connectors" => Some(Self::Connector),
            _ => None,
        }
    }

    /// Infer a named component's kind from the members its config carries.
    fn infer(config: &Value) -> Self {
        let has = |key: &str| config.get(key).is_some();
        if has("$namespace") || has("contract") || has("namespace") || has("functions") || has("tokens") {
            Self::Namespace
        } else if has("algorithms") || has("operations") || has("$provider") || has("protection") || has("generate") {
            Self::KeyProvider
        } else if has("capabilities") {
            Self::Connector
        } else {
            Self::Other
        }
    }
}

/// One provisioned host component: its label, kind, and verbatim behaviour
/// config.
#[derive(Debug, Clone)]
pub struct HostComponent {
    /// The component's label within the case (the map key or its `id`).
    pub label: String,
    /// The component's kind.
    pub kind: HostKind,
    /// The component's configuration, verbatim.
    pub config: Value,
}

/// A case's provisioned host components, flattened across both block forms.
#[derive(Debug, Clone, Default)]
pub struct HostsConfig {
    /// Every provisioned component, in block order.
    pub components: Vec<HostComponent>,
}

impl HostsConfig {
    /// The typed hosts config of a case, or an empty config when it provisions
    /// none.
    #[must_use]
    pub fn from_case(case: &Case) -> Self {
        case.hosts.as_ref().map(Self::parse).unwrap_or_default()
    }

    /// Parse a `hosts` block value into typed components.
    #[must_use]
    pub fn parse(hosts: &Value) -> Self {
        let mut components = Vec::new();
        if let Some(map) = hosts.as_object() {
            for (key, value) in map {
                match HostKind::of_group(key) {
                    Some(kind) => push_group(&mut components, kind, value),
                    None => components.push(HostComponent {
                        label: key.clone(),
                        kind: HostKind::infer(value),
                        config: value.clone(),
                    }),
                }
            }
        }
        Self { components }
    }

    /// The components of one kind.
    pub fn of_kind(&self, kind: HostKind) -> impl Iterator<Item = &HostComponent> {
        self.components.iter().filter(move |c| c.kind == kind)
    }
}

/// Flatten a grouped bucket (`namespaces`/`key_providers`/`connectors`), which
/// is either a label→config map or an array whose elements carry an `id`.
fn push_group(out: &mut Vec<HostComponent>, kind: HostKind, value: &Value) {
    match value {
        Value::Object(map) => {
            for (label, config) in map {
                out.push(HostComponent { label: label.clone(), kind, config: config.clone() });
            }
        }
        Value::Array(items) => {
            for (index, config) in items.iter().enumerate() {
                let label = config
                    .get("id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| index.to_string());
                out.push(HostComponent { label, kind, config: config.clone() });
            }
        }
        _ => {}
    }
}
