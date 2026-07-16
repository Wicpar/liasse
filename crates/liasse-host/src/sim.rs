//! Simulated host components for driving scenarios without real namespaces,
//! providers, or storage backends (the `liasse-store` `contract_tests`
//! precedent: a public testing module shipped with the crate it exercises).
//!
//! Each double is *scriptable* to the behaviours the conformance corpus names,
//! so the future scenario executor can express `provider_set`/`connector_set`/
//! namespace-`op` vocabulary against the real contract types. Behaviours cover:
//! well-behaved operation; wrong-typed and drifting nonconforming returns
//! (SPEC-ISSUES items 15/16); tampered downloads and corrupt objects; and
//! unavailable/failing/hanging components. A hang is a typed budget-exhausting
//! outcome, never an actual infinite loop — real hanging is untestable, so the
//! double returns [`crate::ProviderFailure::WouldNotReturn`] instead.

mod connector;
mod namespace;
mod provider;

pub use connector::{ConnectorOp, SimConnector};
pub use namespace::{Behavior, SimNamespace, SimNamespaceBuilder};
pub use provider::{ProviderOp, SimKeyProvider};
