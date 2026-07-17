//! Blob put/get as driver-facing host operations (SPEC.md §18).
//!
//! The runtime [`BlobEngine`] carries the whole §18 machinery — descriptor
//! acceptance against the streamed bytes, placement-policy planning, transactional
//! upload, and integrity-verified fetch over host [`BlobConnector`]s. It is keyed
//! by *descriptor*, though, and a client speaks in terms of *content*: put these
//! bytes as this media type, then get the content back by its digest.
//!
//! [`BlobHost`] is that content-addressed façade. It owns a configured
//! [`BlobEngine`] together with one field's accepted-type constraints (§18.2) and
//! placement policy (§18.4), retains each committed [`Blob`] under its canonical
//! `$sha512`, and exposes:
//!
//! - [`BlobHost::put`] / [`BlobHost::put_declared`] — the §18.7 upload, returning a
//!   [`BlobPutOutcome`] that reports the committed digest and verified stores, or
//!   the typed rejection of a lying/oversized/unaccepted descriptor;
//! - [`BlobHost::get`] — the §18.8/§18.9 fetch by digest through a visibility gate,
//!   returning a [`BlobGetOutcome`] that never surfaces tampered bytes;
//! - [`BlobHost::fetch_projected`] — the §18.8 fetch gate over the *value* a
//!   caller's surface projection resolved at the descriptor occurrence, so a
//!   metadata-only or hidden projection grants no fetch;
//! - [`BlobHost::placement_state`] — the §18.5 `$stored`/`$satisfied`/`$surplus`
//!   observations of a committed blob against the current placement policy;
//! - [`BlobHost::reconcile`] — the §18.6 convergence of a retained blob's placement.
//!
//! [`BlobConnector`]: liasse_host::BlobConnector

use std::collections::BTreeMap;

use liasse_host::{BlobConnector, BlobIntegrity};
use liasse_runtime::{
    AcceptedType, Blob, BlobEngine, DeclaredDescriptor, FetchError, Placement, PlacementState,
    StoreId, UploadError, Value,
};

/// The result of a §18.7 upload through [`BlobHost::put`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobPutOutcome {
    /// The upload verified and committed: the canonical `$sha512` identifying the
    /// content and the stores now holding a verified copy (`blob.$stored`, §18.5).
    Committed {
        /// The committed content digest (canonical lowercase-hex `$sha512`).
        digest: String,
        /// The stores holding a verified copy.
        stored: Vec<StoreId>,
    },
    /// The upload was rejected before any verified copy landed (§18.2/§18.7).
    Rejected(UploadError),
}

/// The result of a §18.8/§18.9 fetch through [`BlobHost::get`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobGetOutcome {
    /// The exact bytes identified by `$sha512`, verified before delivery.
    Delivered(Vec<u8>),
    /// The surface grants no blob fetch — a metadata-only projection or a revoked
    /// authorization (§18.8).
    Denied,
    /// No verified holder could deliver hash-clean content (§18.9). A tampered or
    /// downed holder never surfaces its bytes as a success.
    NoCleanHolder,
    /// No blob with that digest has been committed on this host.
    Unknown,
}

/// A content-addressed §18 blob façade over a configured [`BlobEngine`], one
/// field's accepted-type and placement policy, and the committed blobs it retains.
/// Generic over the connector implementation `C`, so a deployment binds one
/// connector type with no boxing (heterogeneous connectors compose behind an enum).
pub struct BlobHost<C> {
    engine: BlobEngine<C>,
    accepted: AcceptedType,
    placement: Placement,
    committed: BTreeMap<String, Blob>,
}

impl<C: BlobConnector> BlobHost<C> {
    /// Build a blob host over a configured `engine` (its connectors and store rows
    /// already registered), for a field with the given `accepted` constraints and
    /// `placement` policy.
    #[must_use]
    pub fn new(engine: BlobEngine<C>, accepted: AcceptedType, placement: Placement) -> Self {
        Self { engine, accepted, placement, committed: BTreeMap::new() }
    }

    /// Put `bytes` as media type `media` (§18.7): build the truthful descriptor
    /// the content implies, run the upload, and — on success — retain the committed
    /// blob under its digest for later [`get`](Self::get).
    pub fn put(&mut self, bytes: &[u8], media: &str) -> BlobPutOutcome {
        let declared = DeclaredDescriptor {
            sha512: BlobIntegrity::digest_hex(bytes),
            bytes: bytes.len() as u64,
            media: media.to_owned(),
            name: None,
        };
        self.put_declared(&declared, bytes)
    }

    /// Put `bytes` under a client-`declared` descriptor (§18.7): the descriptor is
    /// verified against the streamed bytes and the accepted type before any copy is
    /// made, so a lying `$sha512`/`$bytes`/`$media` rejects the upload. On success
    /// the committed blob is retained under its verified digest.
    pub fn put_declared(&mut self, declared: &DeclaredDescriptor, bytes: &[u8]) -> BlobPutOutcome {
        match self.engine.upload(declared, &self.accepted, &self.placement, bytes) {
            Ok(blob) => {
                let digest = blob.descriptor().sha512().to_canonical_text();
                let stored = blob.stored();
                self.committed.insert(digest.clone(), blob);
                BlobPutOutcome::Committed { digest, stored }
            }
            Err(error) => BlobPutOutcome::Rejected(error),
        }
    }

    /// Fetch the content identified by `digest` through a §18.8 visibility gate
    /// (`visible` is the authorization decision: a metadata-only projection or a
    /// revoked grant is `false`). Each verified holder is probed in `$serve` order
    /// and its bytes verified before delivery, so tampered content never surfaces.
    #[must_use]
    pub fn get(&self, digest: &str, visible: bool) -> BlobGetOutcome {
        let Some(blob) = self.committed.get(digest) else {
            return BlobGetOutcome::Unknown;
        };
        match self.engine.fetch(blob, visible) {
            Ok(bytes) => BlobGetOutcome::Delivered(bytes),
            Err(FetchError::Denied) => BlobGetOutcome::Denied,
            Err(FetchError::NoCleanHolder) => BlobGetOutcome::NoCleanHolder,
        }
    }

    /// Fetch the content the caller's surface projection exposes (§18.8): the
    /// §18.8 gate is *visibility of a blob value through a currently authorized
    /// surface*, so the caller supplies not a bare digest but the value its
    /// resolved projection yields at the descriptor occurrence.
    ///
    /// - `Some(Value::Blob(descriptor))` — the projection exposes the blob
    ///   *value*; its `$sha512` identifies the content to fetch. A known-hash
    ///   attacker whose surface hides the row, or a revoked member whose role
    ///   view no longer resolves it, never reaches this arm (their projection
    ///   yields `None`).
    /// - `Some(_)` — the projection is metadata-only (a `$bytes`/`$media` value,
    ///   not the blob value): it "grants that metadata and no blob fetch" (§18.8).
    /// - `None` — no descriptor occurrence is visible through the projection.
    ///
    /// Resolving the caller's projection to that value (authentication, scoped
    /// role membership, surface projection, descriptor occurrence) is the
    /// surface view engine's job; this method is the §18.8 gate over its result.
    #[must_use]
    pub fn fetch_projected(&self, projected: Option<&Value>) -> BlobGetOutcome {
        let Some(Value::Blob(descriptor)) = projected else {
            // Metadata-only projection or no visible occurrence: no blob fetch.
            return BlobGetOutcome::Denied;
        };
        self.get(&descriptor.sha512().to_canonical_text(), true)
    }

    /// The §18.5 placement observations of the content `digest`, evaluated
    /// against the host's declared policy — `$stored`, `$satisfied`, `$surplus`.
    /// `None` if no blob with that digest is retained.
    #[must_use]
    pub fn placement_state(&self, digest: &str) -> Option<PlacementState> {
        self.placement_state_under(digest, &self.placement)
    }

    /// The §18.5 placement observations of `digest` evaluated against a
    /// `policy` re-resolved from current store rows (§18.4/§18.5): disabling a
    /// store shrinks the store view the policy resolves to, so a still-verified
    /// copy in a no-longer-required store surfaces in `$surplus` without any
    /// bytes moving. `None` if no blob with that digest is retained.
    #[must_use]
    pub fn placement_state_under(&self, digest: &str, policy: &Placement) -> Option<PlacementState> {
        self.committed.get(digest).map(|blob| blob.placement_state(policy))
    }

    /// Converge a retained blob's placement toward the host's policy (§18.6):
    /// demote holders that no longer verify and repair from a verified source.
    /// Returns whether a blob with that digest is retained.
    pub fn reconcile(&mut self, digest: &str) -> bool {
        let Some(mut blob) = self.committed.remove(digest) else {
            return false;
        };
        self.engine.reconcile(&mut blob, &self.placement);
        self.committed.insert(digest.to_owned(), blob);
        true
    }

    /// The verified stores holding the content `digest`, if it is retained
    /// (`blob.$stored`, §18.5).
    #[must_use]
    pub fn stored(&self, digest: &str) -> Option<Vec<StoreId>> {
        self.committed.get(digest).map(Blob::stored)
    }

    /// The committed blob's complete descriptor as an application value
    /// (§18.1), if `digest` is retained — the verified [`liasse_runtime::Value`]
    /// a §18.7 upload binds to a mutation's blob parameter.
    #[must_use]
    pub fn descriptor_value(&self, digest: &str) -> Option<liasse_runtime::Value> {
        self.committed
            .get(digest)
            .map(|blob| liasse_runtime::Value::Blob(Box::new(blob.descriptor().clone())))
    }

    /// Mutable access to a registered connector, for the §18.12 `connector_set`
    /// fault-injection vocabulary a driver scripts (unavailability, corruption, a
    /// tampering read transport).
    pub fn connector_mut(&mut self, name: &str) -> Option<&mut C> {
        self.engine.connector_mut(name)
    }
}
