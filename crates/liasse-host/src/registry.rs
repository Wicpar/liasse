//! The component registry: the set of host namespaces, key providers, and blob
//! connectors a context has registered, and typed resolution of a package's
//! requirements against it (§16.2, §9.2 step 4, §2.1).
//!
//! Resolution failures are typed — [`ResolutionError::Missing`] versus
//! [`ResolutionError::Incompatible`] versus [`ResolutionError::Ambiguous`] — so
//! the model/runtime can reject a package *before activation* and explain which
//! kind of failure occurred (§16.2: "Missing, incompatible, or ambiguous
//! requirements reject loading before the package becomes active").

use std::collections::BTreeMap;

use crate::connector::BlobConnector;
use crate::descriptor::InterfaceHash;
use crate::namespace::HostNamespace;
use crate::provider::KeyProvider;
use crate::version::{ContractRef, Version};

/// The registered host components of one context.
///
/// Namespaces are held as a *list*: a context MAY register several descriptors,
/// including conflicting ones, and resolution decides between them. Providers
/// and connectors are keyed by the registration name a declaration refers to
/// (`$provider`, a `stores` row's `connector`).
#[derive(Default)]
pub struct Registry {
    namespaces: Vec<Box<dyn HostNamespace>>,
    providers: BTreeMap<String, Box<dyn KeyProvider>>,
    connectors: BTreeMap<String, Box<dyn BlobConnector>>,
}

impl Registry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a host namespace descriptor + implementation.
    pub fn register_namespace(&mut self, namespace: Box<dyn HostNamespace>) {
        self.namespaces.push(namespace);
    }

    /// Register a key provider under the name a `$provider` refers to.
    pub fn register_provider(&mut self, name: impl Into<String>, provider: Box<dyn KeyProvider>) {
        self.providers.insert(name.into(), provider);
    }

    /// Register a blob connector under the name a `stores` row's `connector`
    /// carries.
    pub fn register_connector(
        &mut self,
        name: impl Into<String>,
        connector: Box<dyn BlobConnector>,
    ) {
        self.connectors.insert(name.into(), connector);
    }

    /// Resolve a `$requires` reference to exactly one registered namespace,
    /// applying the Annex E.8 major-compatibility rule.
    ///
    /// - No descriptor of the contract at any version → [`ResolutionError::Missing`].
    /// - Descriptor(s) of the contract exist but none in the required major →
    ///   [`ResolutionError::Incompatible`].
    /// - More than one *distinct* descriptor satisfies the requirement (a
    ///   different interface hash, or a different compatible version) → an
    ///   [`ResolutionError::Ambiguous`] with no single reading (§16.2).
    /// - Exactly one distinct descriptor satisfies it → that namespace.
    pub fn resolve_namespace(
        &self,
        required: &ContractRef,
    ) -> Result<&dyn HostNamespace, ResolutionError> {
        let mut satisfying: Vec<&dyn HostNamespace> = Vec::new();
        let mut same_contract: Vec<Version> = Vec::new();
        for namespace in &self.namespaces {
            let descriptor = namespace.descriptor();
            if descriptor.id() != required.name() {
                continue;
            }
            same_contract.push(descriptor.version());
            if required.accepts(descriptor.id(), descriptor.version()) {
                satisfying.push(namespace.as_ref());
            }
        }

        if satisfying.is_empty() {
            return if same_contract.is_empty() {
                Err(ResolutionError::Missing {
                    required: required.clone(),
                })
            } else {
                Err(ResolutionError::Incompatible {
                    required: required.clone(),
                    found: same_contract,
                })
            };
        }

        // Collapse byte-identical registrations (same version + interface hash):
        // duplicates of one descriptor are not an ambiguity, distinct ones are.
        let mut distinct: Vec<(Version, InterfaceHash)> = Vec::new();
        for namespace in &satisfying {
            let descriptor = namespace.descriptor();
            let identity = (descriptor.version(), descriptor.interface_hash().clone());
            if !distinct.contains(&identity) {
                distinct.push(identity);
            }
        }
        if distinct.len() > 1 {
            return Err(ResolutionError::Ambiguous {
                required: required.clone(),
                candidates: distinct,
            });
        }
        satisfying
            .first()
            .copied()
            .ok_or_else(|| ResolutionError::Missing {
                required: required.clone(),
            })
    }

    /// A registered provider by name, or `None` if unregistered (§2.1: a
    /// missing provider fails validation before activation).
    #[must_use]
    pub fn provider(&self, name: &str) -> Option<&dyn KeyProvider> {
        self.providers.get(name).map(Box::as_ref)
    }

    /// A registered provider by name for a lifecycle operation (`generate`,
    /// `bind`, `disable`, `destroy`), which mutate the provider keystore.
    pub fn provider_mut(&mut self, name: &str) -> Option<&mut (dyn KeyProvider + 'static)> {
        self.providers.get_mut(name).map(Box::as_mut)
    }

    /// A registered connector by name, or `None` if unregistered.
    #[must_use]
    pub fn connector(&self, name: &str) -> Option<&dyn BlobConnector> {
        self.connectors.get(name).map(Box::as_ref)
    }

    /// A registered connector by name for an upload/delete operation.
    pub fn connector_mut(&mut self, name: &str) -> Option<&mut (dyn BlobConnector + 'static)> {
        self.connectors.get_mut(name).map(Box::as_mut)
    }
}

/// A typed namespace-resolution failure (§16.2, §9.2). Distinguishes missing
/// from incompatible so the runtime can report the right diagnostic phase and a
/// package is rejected before activation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolutionError {
    /// No descriptor of the required contract is registered at any version.
    #[error("requirement `{required}` resolves to no registered descriptor")]
    Missing {
        /// The unmet requirement.
        required: ContractRef,
    },
    /// Descriptor(s) of the contract exist, but none within the required major.
    #[error("requirement `{required}` is not satisfied by any registered major")]
    Incompatible {
        /// The unmet requirement.
        required: ContractRef,
        /// The versions of that contract that are registered.
        found: Vec<Version>,
    },
    /// More than one distinct descriptor satisfies the requirement, and no
    /// single reading resolves the choice.
    #[error("requirement `{required}` is ambiguous across {} distinct descriptors", candidates.len())]
    Ambiguous {
        /// The ambiguous requirement.
        required: ContractRef,
        /// The distinct `(version, interface hash)` candidates.
        candidates: Vec<(Version, InterfaceHash)>,
    },
}
