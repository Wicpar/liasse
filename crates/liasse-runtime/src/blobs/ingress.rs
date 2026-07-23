//! Engine-integrated blob ingress and serve state (§18.3/§18.7/§18.8).
//!
//! [`BlobIngress`] is the staged, verified transport value. It owns bytes and a
//! semantic descriptor but has no connector or application-state effect. A
//! [`StagedBlob`] exists only after mutation checks pass and verified copies
//! land; [`BlobCatalog::commit`] makes it serveable only after the state-store
//! transaction commits.

use std::collections::{BTreeMap, BTreeSet};

use liasse_host::{BlobIntegrity, Capability, ConnectorFailure, VerifiedFetchError};
use liasse_value::{BlobDescriptor, Sha512};

use crate::compiled::ResolvedBlobPolicy;
use crate::error::{Rejection, RejectionReason};
use crate::host::HostBinding;

use super::placement::dedup;
use super::{Blob, CopyState, FetchError, Placement, PlacementState, Store, StoreId, UploadError};

/// Bytes staged at the hostile-input boundary and proven to match one accepted
/// blob field. Construction is private to [`Engine::stage_blob`](crate::Engine::stage_blob).
pub struct BlobIngress {
    pub(crate) mutation: String,
    pub(crate) parameter: String,
    pub(crate) descriptor: BlobDescriptor,
    pub(crate) bytes: Vec<u8>,
}

impl BlobIngress {
    /// The verified descriptor the caller binds to the mutation parameter.
    #[must_use]
    pub fn descriptor(&self) -> &BlobDescriptor {
        &self.descriptor
    }
}

/// A successful verified serve read and the holder order used to obtain it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobFetch {
    bytes: Vec<u8>,
    holders: Vec<StoreId>,
}

impl BlobFetch {
    /// Exact bytes matching the descriptor's SHA-512 identity.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Accessible verified holders in `$serve` order.
    #[must_use]
    pub fn holders(&self) -> &[StoreId] {
        &self.holders
    }
}

/// Engine-owned logical blob placements that have crossed the application-state
/// commit boundary. Physical objects absent from this catalog are staged or
/// orphaned transport objects and can never be served.
#[derive(Default)]
pub(crate) struct BlobCatalog {
    committed: BTreeMap<Sha512, CatalogEntry>,
}

struct CatalogEntry {
    blob: Blob,
    connectors: BTreeMap<StoreId, String>,
}

/// Verified landed copies awaiting the state-store transaction outcome.
pub(crate) struct StagedBlob {
    blob: Blob,
    connectors: BTreeMap<StoreId, String>,
    created: Vec<(String, Sha512)>,
}

impl StagedBlob {
    pub(crate) fn placement_state(&self) -> PlacementState {
        let verified: BTreeSet<StoreId> = self.blob.stored().into_iter().collect();
        PlacementState {
            stored: verified.iter().cloned().collect(),
            satisfied: true,
            surplus: Vec::new(),
        }
    }

    pub(crate) fn digest(&self) -> Sha512 {
        *self.blob.descriptor.sha512()
    }

    /// Best-effort removal of objects this admission newly created. Failure to
    /// delete leaves an uncommitted transport object, which §18.7 permits a
    /// sweeper to reap; it remains absent from [`BlobCatalog`] and unserveable.
    pub(crate) fn rollback(self, host: &mut HostBinding) {
        for (connector, digest) in self.created.into_iter().rev() {
            if let Some(connector) = host.connector_mut(&connector) {
                let _ = connector.delete(&digest);
            }
        }
    }
}

impl BlobCatalog {
    pub(crate) fn land(
        host: &mut HostBinding,
        ingress: &BlobIngress,
        resolved: ResolvedBlobPolicy,
    ) -> Result<StagedBlob, UploadError> {
        let digest = *ingress.descriptor.sha512();
        let plan = writable_plan(resolved.policy.plan(), &resolved.stores, host, &digest)
            .ok_or(UploadError::NoWritablePlacement)?;
        let mut placement = BTreeMap::new();
        let mut connectors = BTreeMap::new();
        let mut created = Vec::new();
        for store in &plan {
            let result = land_copy(
                store,
                &resolved.stores,
                host,
                &digest,
                &ingress.bytes,
                &mut created,
            );
            if let Err(error) = result {
                rollback_created(host, created);
                return Err(error);
            }
            let Some(row) = resolved.stores.get(store) else {
                rollback_created(host, created);
                return Err(UploadError::NoWritablePlacement);
            };
            placement.insert(store.clone(), CopyState::Verified);
            connectors.insert(store.clone(), row.connector.clone());
        }
        let verified = placement.keys().cloned().collect();
        let serve = resolved.policy.serve_order(&verified);
        Ok(StagedBlob {
            blob: Blob {
                descriptor: ingress.descriptor.clone(),
                placement,
                serve,
            },
            connectors,
            created,
        })
    }

    pub(crate) fn commit(&mut self, staged: StagedBlob) {
        let digest = staged.digest();
        match self.committed.get_mut(&digest) {
            Some(existing) => {
                existing.blob.placement.extend(staged.blob.placement);
                for store in staged.blob.serve {
                    if !existing.blob.serve.contains(&store) {
                        existing.blob.serve.push(store);
                    }
                }
                existing.connectors.extend(staged.connectors);
            }
            None => {
                self.committed.insert(
                    digest,
                    CatalogEntry {
                        blob: staged.blob,
                        connectors: staged.connectors,
                    },
                );
            }
        }
    }

    pub(crate) fn fetch(
        &self,
        host: &HostBinding,
        descriptor: &BlobDescriptor,
    ) -> Result<BlobFetch, FetchError> {
        self.fetch_digest(host, descriptor.sha512())
    }

    pub(crate) fn fetch_digest(
        &self,
        host: &HostBinding,
        digest: &Sha512,
    ) -> Result<BlobFetch, FetchError> {
        let entry = self.committed.get(digest).ok_or(FetchError::Unknown)?;
        let integrity = BlobIntegrity::new(*digest);
        for store in &entry.blob.serve {
            if entry.blob.placement.get(store) != Some(&CopyState::Verified) {
                continue;
            }
            let Some(name) = entry.connectors.get(store) else {
                continue;
            };
            let Some(connector) = host.connector(name) else {
                continue;
            };
            match integrity.fetch_verified(connector) {
                Ok(bytes) => {
                    return Ok(BlobFetch {
                        bytes,
                        holders: entry.blob.serve.clone(),
                    });
                }
                Err(VerifiedFetchError::Tampered(_) | VerifiedFetchError::Connector(_)) => {}
            }
        }
        Err(FetchError::NoCleanHolder)
    }

    pub(crate) fn stored(&self, digest: &Sha512) -> Option<Vec<StoreId>> {
        self.committed.get(digest).map(|entry| entry.blob.stored())
    }
}

/// §18.3 eager validation of placement-reachable rows. Missing registrations
/// and upload capability shortfalls are host rejections, never silent routing
/// omissions.
pub(crate) fn validate_connectors(stores: &[Store], host: &HostBinding) -> Result<(), Rejection> {
    for store in stores.iter().filter(|store| store.enabled) {
        let connector = host.connector(&store.connector).ok_or_else(|| {
            Rejection::new(
                RejectionReason::Host,
                format!(
                    "blob store `{}` selects unregistered connector `{}` (§18.3)",
                    store.id.as_str(),
                    store.connector
                ),
            )
        })?;
        let capabilities = connector.capabilities();
        if !capabilities.has(Capability::StreamUpload)
            || !capabilities.has(Capability::StreamDownload)
        {
            return Err(Rejection::new(
                RejectionReason::Host,
                format!(
                    "blob store `{}` connector `{}` lacks streamed upload/download capability",
                    store.id.as_str(),
                    store.connector
                ),
            ));
        }
    }
    Ok(())
}

fn writable_plan(
    placement: &Placement,
    stores: &BTreeMap<StoreId, Store>,
    host: &HostBinding,
    digest: &Sha512,
) -> Option<Vec<StoreId>> {
    match placement {
        Placement::View(ids) => {
            let ids = dedup(ids);
            ids.iter()
                .all(|store| writable(store, stores, host, digest))
                .then_some(ids)
        }
        Placement::All(branches) => {
            let mut required = Vec::new();
            for branch in branches {
                required.extend(writable_plan(branch, stores, host, digest)?);
            }
            Some(dedup(&required))
        }
        Placement::Any(branches) => branches
            .iter()
            .find_map(|branch| writable_plan(branch, stores, host, digest)),
        Placement::Copies { n, of } => {
            let writable: Vec<StoreId> = dedup(of)
                .into_iter()
                .filter(|store| writable(store, stores, host, digest))
                .collect();
            (writable.len() >= *n).then(|| writable.into_iter().take(*n).collect())
        }
    }
}

fn writable(
    store: &StoreId,
    stores: &BTreeMap<StoreId, Store>,
    host: &HostBinding,
    digest: &Sha512,
) -> bool {
    let Some(store) = stores.get(store) else {
        return false;
    };
    if !store.enabled {
        return false;
    }
    host.connector(&store.connector).is_some_and(|connector| {
        let capabilities = connector.capabilities();
        capabilities.has(Capability::StreamUpload)
            && capabilities.has(Capability::StreamDownload)
            && connector.exists(digest).is_ok()
    })
}

fn land_copy(
    store: &StoreId,
    stores: &BTreeMap<StoreId, Store>,
    host: &mut HostBinding,
    digest: &Sha512,
    bytes: &[u8],
    created: &mut Vec<(String, Sha512)>,
) -> Result<(), UploadError> {
    let row = stores.get(store).ok_or(UploadError::NoWritablePlacement)?;
    let connector = host
        .connector_mut(&row.connector)
        .ok_or_else(|| connector_error(&row.connector, "connector is not registered"))?;
    let existed = connector
        .exists(digest)
        .map_err(|error| connector_failure(&row.connector, error))?;
    let integrity = BlobIntegrity::new(*digest);
    if existed && integrity.fetch_verified(&*connector).is_ok() {
        return Ok(());
    }
    connector
        .upload(digest, bytes)
        .map_err(|error| connector_failure(&row.connector, error))?;
    if !existed
        && !created
            .iter()
            .any(|(name, created_digest)| name == &row.connector && created_digest == digest)
    {
        created.push((row.connector.clone(), *digest));
    }
    integrity
        .fetch_verified(&*connector)
        .map_err(|_| UploadError::CopyVerification)?;
    Ok(())
}

fn rollback_created(host: &mut HostBinding, created: Vec<(String, Sha512)>) {
    for (name, digest) in created.into_iter().rev() {
        if let Some(connector) = host.connector_mut(&name) {
            let _ = connector.delete(&digest);
        }
    }
}

fn connector_failure(connector: &str, failure: ConnectorFailure) -> UploadError {
    connector_error(connector, &failure.to_string())
}

fn connector_error(connector: &str, reason: &str) -> UploadError {
    UploadError::Connector {
        connector: connector.to_owned(),
        reason: reason.to_owned(),
    }
}
