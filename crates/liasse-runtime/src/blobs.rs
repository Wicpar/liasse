//! Blob dynamic semantics (§18): the descriptor as an application value, the
//! placement-policy plan, transactional upload, and integrity-verified fetch,
//! over host-supplied [`BlobConnector`](liasse_host::BlobConnector)s.
//!
//! The logical model keeps identity, placement, and integrity observable while
//! connectors perform physical transfer:
//!
//! - **Descriptor acceptance** (§18.1/§18.2): a client-declared descriptor is
//!   verified against the streamed bytes — exact SHA-512, exact byte count,
//!   accepted media, and `$max_bytes` — before any copy is made. A malformed or
//!   lying descriptor rejects the call before its transition.
//! - **Placement policy** (§18.4): `$in` is a plan of `view`/`$all`/`$any`/
//!   `$copies` over stores. A new write chooses the first branch whose complete
//!   requirements can be fulfilled and rejects when none can; `$serve` is the
//!   flattened depth-first order with duplicate store identities removed.
//! - **Transactional upload** (§18.7): every required verified copy of one
//!   complete branch must land — each verified at its destination — or the whole
//!   upload is rejected with no partial verified copy.
//! - **Integrity** (§18.8/§18.9): a fetch verifies bytes against `$sha512`
//!   before delivering, so a compromised connector's tampered bytes never
//!   surface as a successful result.

use std::collections::{BTreeMap, BTreeSet};

use liasse_host::{BlobConnector, BlobIntegrity, VerifiedFetchError};
use liasse_value::{BlobDescriptor, MediaType, Sha512};

mod placement;

use placement::dedup;
pub use placement::{CopyState, Placement, PlacementState, Store, StoreId};

/// The accepted-blob-type constraints of a `blob` field (§18.2).
#[derive(Debug, Clone)]
pub struct AcceptedType {
    /// The inclusive content-size limit.
    pub max_bytes: u64,
    /// The set of accepted media types.
    pub media: Vec<MediaType>,
}

/// A committed blob occurrence (§18.5): its descriptor and per-store placement.
#[derive(Debug, Clone)]
pub struct Blob {
    descriptor: BlobDescriptor,
    placement: BTreeMap<StoreId, CopyState>,
    serve: Vec<StoreId>,
}

impl Blob {
    /// The complete descriptor (the application value, §18.1).
    #[must_use]
    pub fn descriptor(&self) -> &BlobDescriptor {
        &self.descriptor
    }

    /// The verified stores holding this content (`blob.$stored`, §18.5).
    #[must_use]
    pub fn stored(&self) -> Vec<StoreId> {
        self.placement
            .iter()
            .filter(|(_, state)| **state == CopyState::Verified)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The placement state of one store, if any (`blob.$placement[store]`).
    #[must_use]
    pub fn placement(&self, store: &StoreId) -> Option<CopyState> {
        self.placement.get(store).copied()
    }

    /// The §18.5 logical placement observations of this occurrence, evaluated
    /// against the current placement `policy`: `$stored` (the verified stores),
    /// `$satisfied` (the policy over them), and `$surplus` (verified copies
    /// outside the currently required policy).
    ///
    /// The `policy` is the *current* resolution of the field's `$blob_storage`
    /// `$in`, re-derived from current store rows — so disabling a store shrinks
    /// the required set without moving bytes, and the now-unrequired verified
    /// copy surfaces in `$surplus` (§18.4/§18.5).
    #[must_use]
    pub fn placement_state(&self, policy: &Placement) -> PlacementState {
        let verified = self.verified_set();
        PlacementState {
            stored: verified.iter().cloned().collect(),
            satisfied: policy.satisfied_by(&verified),
            surplus: policy.surplus(&verified),
        }
    }

    /// The verified store set (`blob.$stored` as a set, §18.5), for evaluating
    /// the placement policy over it.
    fn verified_set(&self) -> BTreeSet<StoreId> {
        self.placement
            .iter()
            .filter(|(_, state)| **state == CopyState::Verified)
            .map(|(id, _)| id.clone())
            .collect()
    }
}

/// A client-declared blob descriptor plus the raw declared members, as they
/// arrive with an upload before verification (§18.7 step 4). A lying or
/// malformed client is modelled by declared members that disagree with the
/// streamed bytes.
#[derive(Debug, Clone)]
pub struct DeclaredDescriptor {
    /// The declared `$sha512` text (may be non-canonical or wrong).
    pub sha512: String,
    /// The declared `$bytes` count.
    pub bytes: u64,
    /// The declared `$media` type.
    pub media: String,
    /// The optional declared `$name`.
    pub name: Option<String>,
}

/// Why an upload was rejected before its transition was admitted (§18.2/§18.7).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UploadError {
    /// The declared `$sha512` was not a valid lowercase-hex SHA-512.
    #[error("declared descriptor sha512 is malformed")]
    MalformedDigest,
    /// The streamed bytes hash to a different digest than declared (§18.1).
    #[error("declared sha512 does not match the streamed bytes")]
    DigestMismatch,
    /// The declared byte count disagrees with the streamed length (§18.1).
    #[error("declared byte count does not match the streamed bytes")]
    ByteCountMismatch,
    /// The media type is not accepted by the field (§18.2).
    #[error("media type `{0}` is not accepted")]
    MediaNotAccepted(String),
    /// The content exceeds the inclusive `$max_bytes` limit (§18.2).
    #[error("content of {actual} bytes exceeds the {limit}-byte limit")]
    TooLarge {
        /// The content size.
        actual: u64,
        /// The declared limit.
        limit: u64,
    },
    /// No complete placement branch could be fulfilled (§18.4).
    #[error("no placement branch can currently be fulfilled")]
    NoWritablePlacement,
    /// A connector failed while landing a required copy (§18.12).
    #[error("connector `{connector}` failed landing a copy: {reason}")]
    Connector {
        /// The connector name.
        connector: String,
        /// The failure detail.
        reason: String,
    },
    /// A copy landed but failed destination verification (§18.9 copy step).
    #[error("a landed copy failed destination verification")]
    CopyVerification,
}

/// Why a fetch could not deliver a hash-clean result (§18.8/§18.9).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    /// The surface grants no blob fetch (metadata-only or no longer authorized).
    #[error("the surface grants no blob fetch")]
    Denied,
    /// No verified holder could deliver bytes matching `$sha512` (§18.9).
    #[error("no holder delivered content matching the descriptor")]
    NoCleanHolder,
}

/// The blob engine (§18.3): registered connectors and store rows, over which
/// uploads and fetches run. Generic over the connector implementation `C` so a
/// deployment binds one connector type (heterogeneous connectors compose behind
/// an enum) and no boxing or downcasting is needed to reconfigure a double.
pub struct BlobEngine<C> {
    connectors: BTreeMap<String, C>,
    stores: Vec<Store>,
}

impl<C: BlobConnector> Default for BlobEngine<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: BlobConnector> BlobEngine<C> {
    /// A fresh engine with no connectors or stores.
    #[must_use]
    pub fn new() -> Self {
        Self { connectors: BTreeMap::new(), stores: Vec::new() }
    }

    /// Register a connector under `name` (§18.12 registration).
    pub fn register(&mut self, name: impl Into<String>, connector: C) {
        self.connectors.insert(name.into(), connector);
    }

    /// Add a store row (§18.3).
    pub fn add_store(&mut self, store: Store) {
        self.stores.push(store);
    }

    /// Mutable access to a registered connector, for host-driven
    /// reconfiguration (the `connector_set` fault-injection vocabulary of
    /// §18.12).
    pub fn connector_mut(&mut self, name: &str) -> Option<&mut C> {
        self.connectors.get_mut(name)
    }

    /// Run the complete §18.7 upload sequence: verify the declared descriptor
    /// against the streamed `bytes` and the accepted type, choose the first
    /// fulfillable placement branch, land and verify every required copy, and
    /// return the committed [`Blob`]. Any failure leaves no partial verified copy
    /// (§18.12).
    pub fn upload(
        &mut self,
        declared: &DeclaredDescriptor,
        accepted: &AcceptedType,
        placement: &Placement,
        bytes: &[u8],
    ) -> Result<Blob, UploadError> {
        let descriptor = self.verify_descriptor(declared, accepted, bytes)?;
        let plan = self
            .writable_plan(placement)
            .ok_or(UploadError::NoWritablePlacement)?;
        let digest = *descriptor.sha512();
        let mut landed = BTreeMap::new();
        for store in &plan {
            self.land_copy(store, &digest, bytes)?;
            landed.insert(store.clone(), CopyState::Verified);
        }
        let serve = self.serve_order(placement, &landed);
        Ok(Blob { descriptor, placement: landed, serve })
    }

    /// Issue and follow a §18.8 fetch plan for `blob`, returning the exact bytes
    /// identified by `$sha512`. `visible` is the §18.8 authorization gate: a
    /// metadata-only projection or a revoked authorization grants no fetch. Each
    /// verified holder is probed in `$serve` order and its bytes are verified
    /// before delivery, so tampered content never surfaces (§18.9).
    pub fn fetch(&self, blob: &Blob, visible: bool) -> Result<Vec<u8>, FetchError> {
        if !visible {
            return Err(FetchError::Denied);
        }
        let integrity = BlobIntegrity::new(*blob.descriptor.sha512());
        for store in &blob.serve {
            if blob.placement.get(store) != Some(&CopyState::Verified) {
                continue;
            }
            let Some(connector) = self.connector_for(store) else { continue };
            match integrity.fetch_verified(connector) {
                Ok(bytes) => return Ok(bytes),
                // A tampered/corrupt holder is skipped; recovery from another
                // verified holder is a client MAY (§18.8, SPEC-ISSUES item 19).
                Err(VerifiedFetchError::Tampered(_) | VerifiedFetchError::Connector(_)) => {}
            }
        }
        Err(FetchError::NoCleanHolder)
    }

    /// Converge `blob`'s placement toward `placement` (§18.6): demote holders
    /// that no longer verify, then copy from a verified source into every plan
    /// store that is not yet verified, verifying each destination.
    pub fn reconcile(&mut self, blob: &mut Blob, placement: &Placement) {
        let digest = *blob.descriptor.sha512();
        self.demote_corrupt(blob, &digest);
        let Some(source) = self.verified_source(blob, &digest) else { return };
        let targets: Vec<StoreId> = self
            .writable_plan(placement)
            .unwrap_or_default()
            .into_iter()
            .filter(|store| blob.placement.get(store) != Some(&CopyState::Verified))
            .collect();
        for store in targets {
            if self.copy_and_verify(store.clone(), &digest, &source) {
                blob.placement.insert(store, CopyState::Verified);
            }
        }
        blob.serve = self.serve_order(placement, &blob.placement);
    }

    // ---- descriptor verification (§18.1/§18.2) ---------------------------

    fn verify_descriptor(
        &self,
        declared: &DeclaredDescriptor,
        accepted: &AcceptedType,
        bytes: &[u8],
    ) -> Result<BlobDescriptor, UploadError> {
        let digest = Sha512::parse(&declared.sha512).map_err(|_| UploadError::MalformedDigest)?;
        // §18.1: the declared hex must be canonical lowercase, and it must match
        // the streamed bytes.
        if declared.sha512 != digest.to_canonical_text() {
            return Err(UploadError::MalformedDigest);
        }
        if BlobIntegrity::digest_hex(bytes) != digest.to_canonical_text() {
            return Err(UploadError::DigestMismatch);
        }
        let actual = bytes.len() as u64;
        if declared.bytes != actual {
            return Err(UploadError::ByteCountMismatch);
        }
        if actual > accepted.max_bytes {
            return Err(UploadError::TooLarge { actual, limit: accepted.max_bytes });
        }
        if !media_accepted(&accepted.media, &declared.media) {
            return Err(UploadError::MediaNotAccepted(declared.media.clone()));
        }
        Ok(BlobDescriptor::new(
            digest,
            declared.bytes,
            MediaType::new(declared.media.clone()),
            declared.name.clone(),
        ))
    }

    // ---- placement (§18.4) -----------------------------------------------

    /// The store set required for a new write: the first fulfillable `$any`
    /// branch, every `$all` branch, or `n` writable stores from a `$copies`
    /// view. `None` when the plan cannot currently be fulfilled.
    fn writable_plan(&self, placement: &Placement) -> Option<Vec<StoreId>> {
        match placement {
            Placement::View(stores) => {
                let dedup = dedup(stores);
                dedup.iter().all(|s| self.writable(s)).then_some(dedup)
            }
            Placement::All(branches) => {
                let mut required = Vec::new();
                for branch in branches {
                    required.extend(self.writable_plan(branch)?);
                }
                Some(dedup(&required))
            }
            Placement::Any(branches) => branches.iter().find_map(|b| self.writable_plan(b)),
            Placement::Copies { n, of } => {
                let writable: Vec<StoreId> =
                    dedup(of).into_iter().filter(|s| self.writable(s)).collect();
                (writable.len() >= *n).then(|| writable.into_iter().take(*n).collect())
            }
        }
    }

    /// The `$serve` order: the flattened placement order restricted to verified
    /// holders (§18.4 default).
    fn serve_order(&self, placement: &Placement, landed: &BTreeMap<StoreId, CopyState>) -> Vec<StoreId> {
        placement
            .flattened()
            .into_iter()
            .filter(|store| landed.get(store) == Some(&CopyState::Verified))
            .collect()
    }

    fn writable(&self, store: &StoreId) -> bool {
        let Some(row) = self.stores.iter().find(|s| &s.id == store) else { return false };
        if !row.enabled {
            return false;
        }
        self.connectors.get(&row.connector).is_some_and(|c| {
            // A store is writable when its connector advertises upload and is
            // reachable now (§18.4 "currently writable"). `observe_usage` needs
            // no digest and fails `Unavailable` on a downed connector (§18.12).
            c.capabilities().has(liasse_host::Capability::StreamUpload)
                && c.observe_usage().is_ok()
        })
    }

    // ---- copying and integrity (§18.9) -----------------------------------

    fn land_copy(&mut self, store: &StoreId, digest: &Sha512, bytes: &[u8]) -> Result<(), UploadError> {
        let connector_name = self.connector_name(store);
        let Some(name) = connector_name else {
            return Err(UploadError::NoWritablePlacement);
        };
        let connector = self
            .connectors
            .get_mut(&name)
            .ok_or(UploadError::NoWritablePlacement)?;
        connector
            .upload(digest, bytes)
            .map_err(|reason| UploadError::Connector { connector: name.clone(), reason: reason.to_string() })?;
        // §18.9 copy step: a destination becomes verified only after its bytes
        // hash clean.
        let integrity = BlobIntegrity::new(*digest);
        integrity
            .fetch_verified(&*connector)
            .map_err(|_| UploadError::CopyVerification)?;
        Ok(())
    }

    fn copy_and_verify(&mut self, store: StoreId, digest: &Sha512, bytes: &[u8]) -> bool {
        // Repair overwrites the destination: clear any demoted/corrupt object
        // first so the fresh copy from a verified source lands clean (§18.6).
        if let Some(name) = self.connector_name(&store)
            && let Some(connector) = self.connectors.get_mut(&name)
        {
            let _ = connector.delete(digest);
        }
        self.land_copy(&store, digest, bytes).is_ok()
    }

    fn demote_corrupt(&self, blob: &mut Blob, digest: &Sha512) {
        let integrity = BlobIntegrity::new(*digest);
        let corrupt: Vec<StoreId> = blob
            .placement
            .iter()
            .filter(|(_, state)| **state == CopyState::Verified)
            .filter(|(store, _)| {
                self.connector_for(store)
                    .is_some_and(|c| integrity.fetch_verified(c).is_err())
            })
            .map(|(store, _)| store.clone())
            .collect();
        for store in corrupt {
            blob.placement.insert(store, CopyState::Corrupt);
        }
    }

    fn verified_source(&self, blob: &Blob, digest: &Sha512) -> Option<Vec<u8>> {
        let integrity = BlobIntegrity::new(*digest);
        blob.placement
            .iter()
            .filter(|(_, state)| **state == CopyState::Verified)
            .find_map(|(store, _)| {
                self.connector_for(store).and_then(|c| integrity.fetch_verified(c).ok())
            })
    }

    fn connector_for(&self, store: &StoreId) -> Option<&C> {
        let name = self.connector_name(store)?;
        self.connectors.get(&name)
    }

    fn connector_name(&self, store: &StoreId) -> Option<String> {
        self.stores.iter().find(|s| &s.id == store).map(|s| s.connector.clone())
    }
}

/// §18.2 media acceptance. Type and subtype are compared case-insensitively
/// after lowercasing; a declaration with parameters compares them exactly after
/// sorting by lowercase name; a declaration without parameters accepts the same
/// type/subtype with any parameters.
fn media_accepted(accepted: &[MediaType], candidate: &str) -> bool {
    let (cand_essence, cand_params) = parse_media(candidate);
    accepted.iter().any(|declared| {
        let (decl_essence, decl_params) = parse_media(declared.as_str());
        if decl_essence != cand_essence {
            return false;
        }
        decl_params.is_empty() || decl_params == cand_params
    })
}

/// Split a media type into its lowercased `type/subtype` essence and its
/// parameters sorted by lowercase name (§18.2).
fn parse_media(media: &str) -> (String, Vec<(String, String)>) {
    let mut parts = media.split(';');
    let essence = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let mut params: Vec<(String, String)> = parts
        .filter_map(|param| {
            let (name, value) = param.split_once('=')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        })
        .collect();
    params.sort();
    (essence, params)
}
