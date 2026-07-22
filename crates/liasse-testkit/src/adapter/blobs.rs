//! Provisioning and driving the §18 blob host components a case declares.
//!
//! A case's `hosts.connectors` block plus its `$model` blob fields and `$data`
//! store rows describe a §18 deployment; [`provision`] reconstructs it as a
//! [`BlobHost`] per blob field. The hosts are owned by the driver (in [`Loaded`],
//! not the surface host) so they persist across a `rebuild_engine`/restart that
//! seals only the engine — the seam the §18.5 placement recording depends on.
//!
//! The driving side stages, fetches, and fault-injects directly against those
//! hosts, admitting an upload through the surface call path (§18.7), fetching
//! through the §18.8 visibility gate over the caller's surface projection (§18.9
//! verifying the bytes), and scripting the §18.12 connector fault vocabulary.
//!
//! An honest upload is a two-phase [`stage`]-then-[`admit`]: [`stage`] streams the
//! bytes into the blob host (verifying byte limit, media, count, and SHA-512),
//! reads the §18.5 placement facts of the just-verified copy, and binds the
//! verified descriptor as the mutation's blob argument; [`admit`] then admits the
//! containing mutation. Splitting the two lets the caller record the placement
//! facts into the engine (`Engine::record_blob_placement`, §18.5) *between* them,
//! so a mutation `return` reading `.file.$satisfied`/`.file.$stored`/
//! `.file.$surplus` resolves them rather than faulting on a placement-index miss.
//! A `claim` models a lying/malformed client: the declared descriptor is verified
//! by [`BlobHost::put_declared`], which rejects a mismatch before any transition
//! (§18.1/§18.2).

use std::collections::BTreeMap;

use liasse_host::sim::{ConnectorOp, SimConnector};
use liasse_host::{BlobIntegrity, Capability, ConnectorCapabilities};
use liasse_store::InstanceStore;
use liasse_surface::{
    AcceptedType, AuthSelection, BlobEngine, BlobGetOutcome, BlobHost, BlobPutOutcome,
    DeclaredDescriptor, Placement, PlacementPolicy, PlacementState, Store, StoreId, Subscription,
    SurfaceAddress, SurfaceCall, SurfaceWatch, Value, ViewResult,
};
use liasse_value::{MediaType, Sha512};
use serde_json::Value as J;

use crate::contract::Observation;
use crate::hosts::{HostKind, HostsConfig};
use crate::outcome::Outcome;

use super::{host_fault, observe_call, wire, AdapterError, Loaded};

/// The composed §18 blob hosts a load owns, one per blob field (mutation-parameter)
/// name. Owned by the driver ([`Loaded`]) rather than the surface host so the staged
/// bytes and connector fault state survive a `rebuild_engine`/restart.
pub(super) type BlobHosts = BTreeMap<String, BlobHost<SimConnector>>;

/// The §18 blob wiring a load reconstructs: the blob-field names, the
/// store→connector map a `connector_set { corrupt }` resolves through, the store
/// rows (§18.3) the placement policy re-resolves against, and the per-field
/// placement-policy source so a shrunk store view (a disabled store, §18.5) is
/// reflected on the next read.
#[derive(Debug, Default)]
pub(super) struct BlobWiring {
    /// The blob field (mutation-parameter) names a blob host was provisioned for.
    pub(super) fields: Vec<String>,
    /// store id → connector name, so a store-view `corrupt` finds its connector.
    store_connector: BTreeMap<String, String>,
    /// The store rows (§18.3) the placement policy re-resolves against: seeded from
    /// `$data`, and updated when a committed mutation flips a store's `enabled` flag
    /// so a later placement read observes the shrunk store view (§18.5).
    stores: Vec<Store>,
    /// Per blob field: the `$blob_storage.$in` policy source (§18.4), re-resolved
    /// against the current [`stores`](Self::stores) each time placement is recorded.
    policy_source: BTreeMap<String, Option<J>>,
    /// Whether the package reads a §18.5 placement member anywhere, so an upload
    /// records the placement facts and a later mutation refreshes them.
    placement_reads: bool,
}

impl BlobWiring {
    /// Whether the package reads a §18.5 placement member (`.$satisfied`/`.$stored`/
    /// `.$surplus`), so the driver records the facts an evaluation resolves against.
    pub(super) fn placement_reads(&self) -> bool {
        self.placement_reads
    }

    /// The §18.4 placement policy of `field`, re-resolved against the current store
    /// rows: disabling a store shrinks the store view the policy yields, so an
    /// already-verified copy in a no-longer-required store becomes surplus (§18.5).
    pub(super) fn resolve_policy(&self, field: &str) -> Placement {
        match self.policy_source.get(field).and_then(Option::as_ref) {
            Some(value) => placement_from_value(value, &self.stores),
            None => Placement::View(self.stores.iter().map(|s| s.id.clone()).collect()),
        }
    }

    /// Absorb a committed mutation's result into the tracked store rows: a returned
    /// store row (its `id` a known store, carrying an `enabled` flag, §18.3) updates
    /// that store's participation so the placement policy re-resolves against it.
    pub(super) fn absorb_store_changes(&mut self, value: &J) {
        match value {
            J::Object(row) => self.absorb_store_row(row),
            J::Array(rows) => {
                for row in rows.iter().filter_map(J::as_object) {
                    self.absorb_store_row(row);
                }
            }
            _ => {}
        }
    }

    fn absorb_store_row(&mut self, row: &serde_json::Map<String, J>) {
        let Some(id) = row.get("id").and_then(J::as_str) else { return };
        let Some(enabled) = row.get("enabled").and_then(J::as_bool) else { return };
        if let Some(store) = self.stores.iter_mut().find(|s| s.id.as_str() == id) {
            store.enabled = enabled;
        }
    }
}

/// A parsed `blob_put` step (tests/18-blobs/NOTES.md).
pub(super) struct BlobPutSpec {
    pub(super) call: String,
    pub(super) param: String,
    pub(super) args: J,
    pub(super) content: Vec<u8>,
    pub(super) media: String,
    pub(super) name: Option<String>,
    pub(super) claim: Option<J>,
    pub(super) operation_id: Option<String>,
    pub(super) connection: String,
    /// The §11.4 per-request authenticator selection the upload runs under, so a
    /// role-scoped blob surface (`member.docs.add`) authenticates before admission
    /// exactly as a plain `call` step does.
    pub(super) auth: Option<AuthSelection>,
}

/// A parsed `blob_get` step (tests/18-blobs/NOTES.md).
pub(super) struct BlobGetSpec {
    /// The surface view address the caller's projection resolves through.
    pub(super) surface: String,
    /// The view parameters (a parameterized surface view reads them as `$params`).
    pub(super) args: J,
    /// The descriptor occurrence within the resolved row (`.file`), naming the
    /// blob field whose value the fetch gate is applied to.
    pub(super) at: Option<String>,
    pub(super) connection: String,
    /// The §11.4 per-request authenticator selection the fetch runs under.
    pub(super) auth: Option<AuthSelection>,
}

/// A parsed `connector_set` step (§18.12 fault injection).
pub(super) struct ConnectorSetSpec {
    pub(super) connector: Option<String>,
    pub(super) available: Option<bool>,
    pub(super) fail: Vec<ConnectorOp>,
    pub(super) corrupt: Option<String>,
}

// ---- provisioning --------------------------------------------------------

/// Reconstruct a [`BlobHost`] for every `$type: blob` field in a `$blob_storage`
/// collection, over the case's `hosts.connectors` and `$data` store rows. Returns
/// the wiring later steps resolve connectors and placement policies through, and
/// the hosts the driver owns (so they survive a `rebuild_engine`/restart).
pub(super) fn provision(package: &J, hosts: Option<&J>) -> (BlobWiring, BlobHosts) {
    let mut wiring = BlobWiring::default();
    let mut blob_hosts = BlobHosts::new();
    let Some(model) = package.get("$model").and_then(J::as_object) else {
        return (wiring, blob_hosts);
    };
    let connectors = connector_specs(hosts);
    if connectors.is_empty() {
        return (wiring, blob_hosts);
    }
    let stores = store_rows(package);
    wiring.store_connector =
        stores.iter().map(|s| (s.id.as_str().to_owned(), s.connector.clone())).collect();
    wiring.stores = stores.clone();
    wiring.placement_reads = reads_placement_member(package);

    for collection in model.values() {
        let Some(collection) = collection.as_object() else { continue };
        let Some(storage) = collection.get("$blob_storage") else { continue };
        let policy_source = storage.get("$in").cloned();
        let policy = policy_of(storage, &stores);
        for (field_name, field) in collection {
            if field.get("$type").and_then(J::as_str) != Some("blob") {
                continue;
            }
            let mut engine = BlobEngine::new();
            for (name, caps, available) in &connectors {
                let mut connector = SimConnector::new(caps.clone());
                if !available {
                    connector.set_available(false);
                }
                engine.register(name.clone(), connector);
            }
            for store in &stores {
                engine.add_store(store.clone());
            }
            blob_hosts.insert(
                field_name.clone(),
                BlobHost::new(engine, accepted_type(field), policy.clone()),
            );
            wiring.fields.push(field_name.clone());
            wiring.policy_source.insert(field_name.clone(), policy_source.clone());
        }
    }
    (wiring, blob_hosts)
}

/// Whether the package reads a §18.5 placement member anywhere — the members the
/// expression layer resolves off the engine's recorded placement ledger, so their
/// presence is what makes the driver record the facts before an evaluation reads
/// them (an unrecorded read is a placement-index miss that faults, §18.5).
fn reads_placement_member(package: &J) -> bool {
    serde_json::to_string(package).is_ok_and(|text| {
        text.contains("$satisfied") || text.contains("$stored") || text.contains("$surplus")
    })
}

/// The `(name, capabilities, available)` of every declared connector.
fn connector_specs(hosts: Option<&J>) -> Vec<(String, ConnectorCapabilities, bool)> {
    let Some(hosts) = hosts else { return Vec::new() };
    HostsConfig::parse(hosts)
        .of_kind(HostKind::Connector)
        .map(|component| {
            let caps = component
                .config
                .get("capabilities")
                .and_then(J::as_array)
                .map(|list| list.iter().filter_map(J::as_str).filter_map(capability_of).collect::<Vec<_>>())
                .unwrap_or_default();
            let available = component.config.get("available").and_then(J::as_bool).unwrap_or(true);
            (component.label.clone(), ConnectorCapabilities::new(caps), available)
        })
        .collect()
}

/// Map a `hosts.connectors` capability token to its [`Capability`] (§18.12).
fn capability_of(token: &str) -> Option<Capability> {
    Some(match token {
        "stream_upload" => Capability::StreamUpload,
        "stream_download" => Capability::StreamDownload,
        "presigned_upload" => Capability::PresignedUpload,
        "presigned_download" => Capability::PresignedDownload,
        "range_reads" => Capability::RangeReads,
        "server_side_copy" => Capability::ServerSideCopy,
        "checksum" => Capability::Checksum,
        "delete" => Capability::Delete,
        "physical_usage" => Capability::PhysicalUsage,
        _ => return None,
    })
}

/// The `$data.stores` rows: each store's id, connector, and enabled flag (default
/// `true`, matching the model field default).
fn store_rows(package: &J) -> Vec<Store> {
    let Some(stores) = package.get("$data").and_then(|d| d.get("stores")).and_then(J::as_object) else {
        return Vec::new();
    };
    stores
        .iter()
        .map(|(id, row)| Store {
            id: StoreId::new(id.clone()),
            connector: row.get("connector").and_then(J::as_str).unwrap_or_default().to_owned(),
            enabled: row.get("enabled").and_then(J::as_bool).unwrap_or(true),
        })
        .collect()
}

/// The accepted-type constraints (§18.2) of a `blob` field: its `$max_bytes`
/// limit and `$media` set.
fn accepted_type(field: &J) -> AcceptedType {
    let max_bytes = field.get("$max_bytes").and_then(as_u64_flexible).unwrap_or(u64::MAX);
    let media = field
        .get("$media")
        .and_then(J::as_array)
        .map(|list| list.iter().filter_map(J::as_str).map(MediaType::new).collect())
        .unwrap_or_default();
    AcceptedType { max_bytes, media }
}

/// The complete placement policy (§18.4) of a `$blob_storage` block: the `$in`
/// plan plus the optional `$serve` preferred read order.
fn policy_of(storage: &J, stores: &[Store]) -> PlacementPolicy {
    PlacementPolicy::new(placement_of(storage, stores), serve_of(storage, stores))
}

/// The placement plan (§18.4) of a `$blob_storage` policy's `$in`.
fn placement_of(storage: &J, stores: &[Store]) -> Placement {
    match storage.get("$in") {
        Some(value) => placement_from_value(value, stores),
        None => Placement::View(stores.iter().map(|s| s.id.clone()).collect()),
    }
}

/// The `$serve` preferred read order (§18.4/§18.8): the flattened store ids of
/// the `$serve` store view, or `None` when the block declares no `$serve` (the
/// serve order then defaults to the flattened `$in` placement order).
fn serve_of(storage: &J, stores: &[Store]) -> Option<Vec<StoreId>> {
    storage.get("$serve").map(|value| placement_from_value(value, stores).flattened())
}

/// A placement leaf (a store-view string) or branch (`$all`/`$any`/`$copies`).
fn placement_from_value(value: &J, stores: &[Store]) -> Placement {
    if let Some(view) = value.as_str() {
        return Placement::View(view_stores(view, stores));
    }
    if let Some(all) = value.get("$all").and_then(J::as_array) {
        return Placement::All(all.iter().map(|v| placement_from_value(v, stores)).collect());
    }
    if let Some(any) = value.get("$any").and_then(J::as_array) {
        return Placement::Any(any.iter().map(|v| placement_from_value(v, stores)).collect());
    }
    if let Some(n) = value.get("$copies").and_then(J::as_u64) {
        let of = value.get("$of").map(|v| view_stores_of(v, stores)).unwrap_or_default();
        return Placement::Copies { n: usize::try_from(n).unwrap_or(usize::MAX), of };
    }
    Placement::View(stores.iter().map(|s| s.id.clone()).collect())
}

/// The stores a `$of` value (a store-view string) yields.
fn view_stores_of(value: &J, stores: &[Store]) -> Vec<StoreId> {
    value.as_str().map(|view| view_stores(view, stores)).unwrap_or_default()
}

/// The stores a §18.4 store-view expression yields. A `/stores['id']` view names
/// one store; a `/stores[:s | s.enabled]` filter yields the enabled stores; any
/// other form yields every store (a conservative superset).
fn view_stores(view: &str, stores: &[Store]) -> Vec<StoreId> {
    let view = view.trim();
    if let Some(rest) = view.strip_prefix("/stores['")
        && let Some(id) = rest.split('\'').next()
    {
        return vec![StoreId::new(id)];
    }
    if view.contains(".enabled") {
        return stores.iter().filter(|s| s.enabled).map(|s| s.id.clone()).collect();
    }
    stores.iter().map(|s| s.id.clone()).collect()
}

/// A `$max_bytes` value, written as a string (canonical `int` wire) or a number.
fn as_u64_flexible(value: &J) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

// ---- driving -------------------------------------------------------------

/// The result of staging a `blob_put`'s bytes into the blob host, before admitting
/// the containing mutation.
pub(super) enum Staged {
    /// The upload rejected before admission (§18.2): oversize, unaccepted media, a
    /// hash/count mismatch, or no writable store. No mutation is admitted.
    Rejected(Observation),
    /// The blob verified and committed in the blob host. Carries the §18.5 placement
    /// facts of the just-verified copy (to record before admission) and the call the
    /// verified descriptor is bound into (§18.7 step 4).
    Ready { digest: String, placement: Option<PlacementState>, call: SurfaceCall },
}

/// Stage a `blob_put`'s bytes into the blob host and build the admission call
/// (§18.7). An honest upload streams its bytes through [`BlobHost::put`], verifying
/// the byte limit, media, count, and SHA-512, then binds the verified descriptor
/// as the mutation's blob argument and reads the §18.5 placement facts of the
/// landed copy; a `claim` declares a descriptor verified before admission (a
/// lying/malformed client rejects, §18.1/§18.2).
pub(super) fn stage<S: InstanceStore>(
    loaded: &mut Loaded<S>,
    spec: &BlobPutSpec,
) -> Result<Staged, AdapterError> {
    let address = SurfaceAddress::parse(&spec.call)
        .map_err(|err| AdapterError::Host(format!("malformed blob call `{}`: {err}", spec.call)))?;
    let types = loaded.routing.arg_types(&spec.call);
    // §12.1 step 3 / §18.2: a blob-mutation argument that does not decode against
    // its declared type is a malformed request, rejected before admission (before
    // any bytes are streamed) rather than coerced to a best-effort inference.
    let Ok(args) = wire::decode_args(&spec.args, &types) else {
        return Ok(Staged::Rejected(Observation::outcome(Outcome::Rejected)));
    };
    let mut call = SurfaceCall::new(address, args);
    if let Some(operation_id) = &spec.operation_id {
        call = call.with_operation_id(operation_id.clone());
    }
    // §11.4: a role-scoped blob surface (`member.docs.add`) authenticates the
    // upload under the step's authenticator selection, exactly as a plain `call`
    // does — without it the containing mutation is denied before its transition.
    if let Some(auth) = &spec.auth {
        call = call.with_auth(auth.clone());
    }

    if let Some(claim) = &spec.claim {
        return stage_declared(loaded, spec, claim);
    }

    let outcome = match loaded.blob_hosts.get_mut(&spec.param) {
        Some(host) => host.put(&spec.content, &spec.media),
        None => {
            return Err(AdapterError::unsupported(
                "`blob_put` names a blob parameter with no composed blob host",
            ))
        }
    };
    let digest = match outcome {
        BlobPutOutcome::Committed { digest, .. } => digest,
        // §18.2: a failed verification rejects the containing call before its state
        // transition — no admission, no placement.
        BlobPutOutcome::Rejected(_) => return Ok(Staged::Rejected(Observation::outcome(Outcome::Rejected))),
    };
    // §18.7 step 4: bind the verified descriptor to the mutation's blob parameter.
    if let Some(value) = loaded.blob_hosts.get(&spec.param).and_then(|host| host.descriptor_value(&digest)) {
        call = call.with_arg(spec.param.clone(), value);
    }
    // §18.5: the placement facts of the just-verified copy, evaluated against the
    // policy re-resolved from the current store rows.
    let policy = loaded.blobs.resolve_policy(&spec.param);
    let placement =
        loaded.blob_hosts.get(&spec.param).and_then(|host| host.placement_state_under(&digest, &policy));
    Ok(Staged::Ready { digest, placement, call })
}

/// Verify a client-declared descriptor (a `claim`) against the streamed bytes
/// (§18.1/§18.2). A negative `$bytes` is a malformed descriptor value the
/// [`DeclaredDescriptor`] type cannot even carry, so it is rejected at the wire
/// boundary. A verifying descriptor that would still need binding into the
/// mutation is a precise seam (the surface admits only honest blob parameters).
fn stage_declared<S: InstanceStore>(
    loaded: &mut Loaded<S>,
    spec: &BlobPutSpec,
    claim: &J,
) -> Result<Staged, AdapterError> {
    if let Some(bytes) = claim.get("$bytes").and_then(J::as_i64)
        && bytes < 0
    {
        // §18.1: `$bytes` is a non-negative integer; a negative claim is a
        // malformed descriptor, rejected before any transition.
        return Ok(Staged::Rejected(Observation::outcome(Outcome::Rejected)));
    }
    let declared = DeclaredDescriptor {
        sha512: claim
            .get("$sha512")
            .and_then(J::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| BlobIntegrity::digest_hex(&spec.content)),
        bytes: claim.get("$bytes").and_then(J::as_u64).unwrap_or(spec.content.len() as u64),
        media: claim.get("$media").and_then(J::as_str).map(ToOwned::to_owned).unwrap_or_else(|| spec.media.clone()),
        name: claim.get("$name").and_then(J::as_str).map(ToOwned::to_owned).or_else(|| spec.name.clone()),
    };
    let Some(host) = loaded.blob_hosts.get_mut(&spec.param) else {
        return Err(AdapterError::unsupported(
            "`blob_put` names a blob parameter with no composed blob host",
        ));
    };
    match host.put_declared(&declared, &spec.content) {
        BlobPutOutcome::Rejected(_) => Ok(Staged::Rejected(Observation::outcome(Outcome::Rejected))),
        BlobPutOutcome::Committed { .. } => Err(AdapterError::unsupported(
            "a client-declared blob descriptor that verifies must be bound into the mutation call, \
             but the surface admits only the honest blob parameter (no declared-descriptor call \
             binding is exposed)",
        )),
    }
}

/// Admit a staged upload's mutation (§18.7), the verified descriptor already bound
/// into `call`. The committed digest is tracked so a later `blob_get`/`connector_set`
/// finds the blob under test.
pub(super) fn admit<S: InstanceStore>(
    loaded: &mut Loaded<S>,
    digests: &mut BTreeMap<String, String>,
    spec: &BlobPutSpec,
    call: SurfaceCall,
) -> Result<Observation, AdapterError> {
    let outcome = loaded.host.call(&spec.connection, &call).map_err(host_fault)?;
    use liasse_surface::SurfaceOutcome as O;
    if matches!(outcome, O::Committed { .. } | O::Unchanged { .. }) {
        digests.insert(spec.param.clone(), BlobIntegrity::digest_hex(&spec.content));
    }
    Ok(observe_call(&outcome))
}

/// The §18.5 placement facts to record for every committed blob, each digest's
/// facts evaluated against its field's policy re-resolved from the current store
/// rows (§18.4/§18.5) — so a store `enabled` change since the upload is reflected.
pub(super) fn placement_records<S: InstanceStore>(
    loaded: &Loaded<S>,
    digests: &BTreeMap<String, String>,
) -> Vec<(String, PlacementState)> {
    digests
        .iter()
        .filter_map(|(field, digest)| {
            let policy = loaded.blobs.resolve_policy(field);
            loaded
                .blob_hosts
                .get(field)
                .and_then(|host| host.placement_state_under(digest, &policy))
                .map(|state| (digest.clone(), state))
        })
        .collect()
}

/// Fetch a blob through the §18.8 visibility gate over the caller's surface
/// projection. The gate is *visibility of a blob value through a currently
/// authorized surface*, so the fetch resolves the caller's surface view (with the
/// step's authenticator and view parameters), reads the value at the descriptor
/// occurrence (`at`, a blob field), and hands it to [`BlobHost::fetch_projected`]:
/// a projected blob *value* grants the fetch of exactly its bytes, while a hidden
/// row (a role-scoped/filtered view that resolves nothing), a revoked membership
/// (the surface denies the read), or a metadata-only projection grants none
/// (§18.8/§18.9).
pub(super) fn get<S: InstanceStore>(
    loaded: &mut Loaded<S>,
    spec: &BlobGetSpec,
) -> Result<Observation, AdapterError> {
    let field = blob_field(spec, loaded).ok_or_else(|| {
        AdapterError::unsupported("`blob_get` step names no descriptor occurrence and no blob field is registered")
    })?;

    // Resolve the caller's surface projection at the current frontier: an
    // authorized read yields the row (and its blob value); a role/membership
    // refusal yields no projection at all, denying the fetch (§18.8).
    let address = SurfaceAddress::parse(&spec.surface)
        .map_err(|err| AdapterError::Host(format!("malformed blob surface `{}`: {err}", spec.surface)))?;
    let arg_types = loaded.routing.view_arg_types(&spec.surface);
    // §12.1 step 3 / Annex A.1: a blob-surface `$params` argument that does not
    // decode against its declared type is a malformed request, rejected rather
    // than coerced to a best-effort inference.
    let Ok(args) = wire::decode_args(&spec.args, &arg_types) else {
        return Ok(Observation::outcome(Outcome::Rejected));
    };
    let mut watch = SurfaceWatch::new(address, format!("$blob_get:{}", spec.surface));
    if !args.is_empty() {
        watch = watch.with_args(args);
    }
    if let Some(auth) = &spec.auth {
        watch = watch.with_auth(auth.clone());
    }
    let subscription = loaded.host.watch(&spec.connection, &watch).map_err(host_fault)?;

    // §18.8: the descriptor occurrence the authorized projection exposes. A blob
    // value grants the fetch; a metadata-only projection, a hidden occurrence, or a
    // refused read (no `Init`) grants none — the fetch plan is not issued.
    let projected: Option<Value> = match &subscription {
        Subscription::Init(result) => projected_value(result, &field).cloned(),
        Subscription::Denied(_) | Subscription::Failed(_) | Subscription::Window(_) => None,
    };
    let Some(host) = loaded.blob_hosts.get(&field) else {
        return Err(AdapterError::unsupported("`blob_get` names a blob field with no composed blob host"));
    };
    Ok(observe_get(&host.fetch_projected(projected.as_ref())))
}

/// The blob-field name a `blob_get` fetches at: the descriptor occurrence `at`
/// (a `.field` path, reduced to its last component), or the sole registered blob
/// field when the step names no occurrence.
fn blob_field<S: InstanceStore>(spec: &BlobGetSpec, loaded: &Loaded<S>) -> Option<String> {
    spec.at
        .as_deref()
        .map(|at| at.trim_start_matches('.').rsplit('.').next().unwrap_or(at).to_owned())
        .filter(|field| !field.is_empty())
        .or_else(|| loaded.blobs.fields.first().cloned())
}

/// The value the resolved projection exposes at blob field `field`: the blob
/// value of the single resolved row, or `None` when the view resolved no row or
/// the projection does not expose that field (a metadata-only or hidden
/// projection). A singular blob-value view resolves at most one row.
fn projected_value<'a>(result: &'a ViewResult, field: &str) -> Option<&'a Value> {
    result.rows().first().and_then(|row| row.field(field))
}

/// Render a §18.8/§18.9 fetch outcome to a harness observation. A delivered
/// result reports `ok` carrying the exact fetched bytes as their staged UTF-8
/// text (in `value`, so a report shows the content) and, in `extra`, the
/// `bytes` (same text) and the `holders` — the §18.8 fetch plan the bytes were
/// served from, the verified holders in `$serve` order — so a case asserting
/// `bytes`/`holders` is checked against the served content and order, not
/// ignored. A denied visibility gate reports `denied`; a no-clean-holder result
/// is a fetch failure (§18.9).
fn observe_get(outcome: &BlobGetOutcome) -> Observation {
    match outcome {
        BlobGetOutcome::Delivered { bytes, holders } => {
            let text = String::from_utf8_lossy(bytes).into_owned();
            let mut observation = Observation::ok(Some(J::String(text.clone())));
            observation.extra.insert("bytes".to_owned(), J::String(text));
            observation.extra.insert(
                "holders".to_owned(),
                J::Array(holders.iter().map(|h| J::String(h.as_str().to_owned())).collect()),
            );
            observation
        }
        BlobGetOutcome::Denied | BlobGetOutcome::Unknown => Observation::outcome(Outcome::Denied),
        BlobGetOutcome::NoCleanHolder => Observation::outcome(Outcome::Error),
    }
}

/// Reconfigure a connector from this step onward (§18.12): full unavailability,
/// clean per-operation failure, or stored-object corruption of the blob under
/// test. The step names a connector directly, or a store view whose connector is
/// resolved through the load's store→connector wiring.
pub(super) fn connector_set<S: InstanceStore>(
    loaded: &mut Loaded<S>,
    digests: &BTreeMap<String, String>,
    spec: &ConnectorSetSpec,
) -> Result<Observation, AdapterError> {
    let target = spec.connector.clone().or_else(|| {
        spec.corrupt
            .as_ref()
            .and_then(|view| view_stores(view, &[]).first().map(StoreId::as_str).map(ToOwned::to_owned))
            .and_then(|store| loaded.blobs.store_connector.get(&store).cloned())
    });
    let Some(target) = target else {
        return Err(AdapterError::unsupported("`connector_set` names neither a connector nor a resolvable store"));
    };
    let corrupt_digests: Vec<Sha512> = spec
        .corrupt
        .as_ref()
        .map(|_| digests.values().filter_map(|hex| Sha512::parse(hex).ok()).collect())
        .unwrap_or_default();
    let fields = loaded.blobs.fields.clone();
    for field in &fields {
        if let Some(host) = loaded.blob_hosts.get_mut(field)
            && let Some(connector) = host.connector_mut(&target)
        {
            if let Some(available) = spec.available {
                connector.set_available(available);
            }
            if !spec.fail.is_empty() {
                connector.set_fail(spec.fail.iter().copied());
            }
            for digest in &corrupt_digests {
                connector.corrupt(*digest);
            }
        }
    }
    Ok(Observation::ok(None))
}
