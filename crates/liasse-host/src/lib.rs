//! Host: the typed contract between a Liasse runtime and its registered Rust
//! host components — host namespaces, key providers, and blob connectors
//! (SPEC.md §16–18, §23) — plus the registry that resolves a package's
//! requirements and the conformance guard that keeps the runtime from trusting
//! a component blindly.
//!
//! # What this crate owns
//!
//! - **Contracts.** [`HostNamespace`] (§16: typed ops over [`liasse_value`]
//!   types, an effect class per function, invoke → typed result or typed
//!   [`InvocationFailure`]), [`KeyProvider`] (§17: capability set, key
//!   generation/rotation/usage ops, public material as typed values), and
//!   [`BlobConnector`] (§18.12: upload, fetch/ranged reads, existence,
//!   connector-reported usage). All object-safe and synchronous — a runtime
//!   holds heterogeneous registrations behind `&dyn` — with no interior
//!   mutability of their own.
//!
//! - **Identity and versioning.** [`ContractName`], [`Version`], and
//!   [`ContractRef`] (`name@major`) carry the §16.2 / Annex E.8 acceptance
//!   rule; [`NamespaceDescriptor`] pins the §16.2 load-time descriptor whose
//!   typed [`OpSignature`]s the model compares against `$requires`.
//!
//! - **[`Registry`].** Components registered under a name/contract, with typed
//!   resolution: [`ResolutionError::Missing`] vs [`ResolutionError::Incompatible`]
//!   vs [`ResolutionError::Ambiguous`], so the model/runtime reject a package
//!   *before activation* (§9.2 step 4, §2.1).
//!
//! - **[`ConformanceGuard`].** A checked-invocation wrapper that validates a
//!   component's returned value against the declared type and detects a `pure`
//!   function drifting, reporting a typed [`ConformanceViolation`]. The runtime
//!   decides policy; §2.1/§16.2 assume conformance and SPEC-ISSUES items 15/16
//!   record what is unpinned about NONCONFORMING components.
//!
//! - **[`BlobIntegrity`].** The §18.9 fetch-verification step, so a compromised
//!   connector's tampered bytes never surface as a successful fetch.
//!
//! - **[`sim`].** Scriptable namespace/provider/connector doubles covering the
//!   corpus behaviour vocabulary, for the future scenario executor.
//!
//! # No interior mutability
//!
//! Where a real component would mutate external state through `&self`, the
//! doubles instead expose a `&mut` reconfiguration/advance surface owned by the
//! caller — no `Cell`/`RefCell`/`Rc`/`Arc`. A never-returning ("hanging")
//! component is modelled as a typed budget-exhausting failure, not a real loop.

mod checked;
mod conform;
mod connector;
mod cose;
mod descriptor;
mod integrity;
mod namespace;
mod provider;
mod registry;
mod version;

pub mod sim;

pub use checked::{ConformanceGuard, ConformanceViolation, GuardError};
pub use conform::{TypeConformance, TypeMismatch};
pub use cose::{cose_descriptor, verify_cose_signature, CoseClaims, CoseToken, SignatureError};
pub use connector::{
    BlobConnector, ByteRange, Capability, CapabilityShortfall as ConnectorCapabilityShortfall,
    ConnectorCapabilities, ConnectorFailure, UsageObservation,
};
pub use descriptor::{
    EffectClass, FunctionDescriptor, InterfaceHash, NamespaceDescriptor, NamespaceType, OpSignature,
};
pub use integrity::{BlobIntegrity, IntegrityMismatch, VerifiedFetchError};
pub use namespace::{HostNamespace, InvocationFailure};
pub use provider::{
    Attestation, CapabilityShortfall, ExternalKeyRef, KeyCapabilities, KeyCapabilitiesBuilder,
    KeyHandle, KeyOperation, KeyProvider, KeySpec, ProtectionClass, ProviderFailure,
    ProviderRequirement, PublicKey, PublicKeyError,
};
pub use registry::{Registry, ResolutionError};
pub use version::{ContractError, ContractName, ContractRef, Version};
