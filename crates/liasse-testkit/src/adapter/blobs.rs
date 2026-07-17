//! Provisioning and driving the §18 blob host components a case declares.
//!
//! A case's `hosts.connectors` block plus its `$model` blob fields and `$data`
//! store rows describe a §18 deployment; [`provision`] reconstructs it as a
//! [`BlobHost`] per blob field, composed into the [`SurfaceHost`] under the field
//! (mutation-parameter) name a `blob_put` step binds to. The driving side
//! ([`put`]/[`get`]/[`connector_set`]) then admits an upload through the surface
//! blob-parameter call path (§18.7), fetches by digest through the §18.9 verify
//! gate (§18.8), and scripts the §18.12 connector fault-injection vocabulary.
//!
//! The bytes-and-media of an honest upload go straight through
//! [`SurfaceHost::call_with_blob`], which stages, verifies, and binds the verified
//! descriptor before admission. A `claim` models a lying/malformed client: the
//! declared descriptor is verified by [`SurfaceHost::blob_put_declared`], which
//! rejects a mismatch before any transition (§18.1/§18.2).

use std::collections::BTreeMap;

use liasse_host::sim::{ConnectorOp, SimConnector};
use liasse_host::{BlobIntegrity, Capability, ConnectorCapabilities};
use liasse_store::InstanceStore;
use liasse_surface::{
    AcceptedType, BlobEngine, BlobHost, BlobPutOutcome, DeclaredDescriptor, Placement, Store,
    StoreId, SurfaceAddress, SurfaceCall, SurfaceHost,
};
use liasse_value::{MediaType, Sha512};
use serde_json::Value as J;

use crate::contract::Observation;
use crate::hosts::{HostKind, HostsConfig};
use crate::outcome::Outcome;

use super::{host_fault, observe_call, wire, AdapterError, Loaded};

/// The §18 blob wiring a load reconstructs: the registered blob-field names and
/// the store→connector map a `connector_set { corrupt }` resolves through.
#[derive(Debug, Clone, Default)]
pub(super) struct BlobWiring {
    /// The blob field (mutation-parameter) names `register_blob` was called with.
    fields: Vec<String>,
    /// store id → connector name, so a store-view `corrupt` finds its connector.
    store_connector: BTreeMap<String, String>,
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
}

/// A parsed `connector_set` step (§18.12 fault injection).
pub(super) struct ConnectorSetSpec {
    pub(super) connector: Option<String>,
    pub(super) available: Option<bool>,
    pub(super) fail: Vec<ConnectorOp>,
    pub(super) corrupt: Option<String>,
}

// ---- provisioning --------------------------------------------------------

/// Reconstruct and compose a [`BlobHost`] for every `$type: blob` field in a
/// `$blob_storage` collection, over the case's `hosts.connectors` and `$data`
/// store rows. Returns the wiring later steps resolve connectors through.
pub(super) fn provision<S: InstanceStore>(
    host: &mut SurfaceHost<S>,
    package: &J,
    hosts: Option<&J>,
) -> BlobWiring {
    let mut wiring = BlobWiring::default();
    let Some(model) = package.get("$model").and_then(J::as_object) else { return wiring };
    let connectors = connector_specs(hosts);
    if connectors.is_empty() {
        return wiring;
    }
    let stores = store_rows(package);
    wiring.store_connector =
        stores.iter().map(|s| (s.id.as_str().to_owned(), s.connector.clone())).collect();

    for collection in model.values() {
        let Some(collection) = collection.as_object() else { continue };
        let Some(storage) = collection.get("$blob_storage") else { continue };
        let placement = placement_of(storage, &stores);
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
            host.register_blob(field_name.clone(), BlobHost::new(engine, accepted_type(field), placement.clone()));
            wiring.fields.push(field_name.clone());
        }
    }
    wiring
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

/// The placement plan (§18.4) of a `$blob_storage` policy's `$in`.
fn placement_of(storage: &J, stores: &[Store]) -> Placement {
    match storage.get("$in") {
        Some(value) => placement_from_value(value, stores),
        None => Placement::View(stores.iter().map(|s| s.id.clone()).collect()),
    }
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

/// Admit a `blob_put` (§18.7). An honest upload streams its bytes through the
/// surface blob-parameter call path; a `claim` declares a descriptor verified
/// before admission (a lying/malformed client rejects, §18.1/§18.2).
pub(super) fn put<S: InstanceStore>(
    loaded: &mut Loaded<S>,
    digests: &mut BTreeMap<String, String>,
    spec: &BlobPutSpec,
) -> Result<Observation, AdapterError> {
    let address = SurfaceAddress::parse(&spec.call)
        .map_err(|err| AdapterError::Host(format!("malformed blob call `{}`: {err}", spec.call)))?;
    let types = loaded.routing.arg_types(&spec.call);
    let args = wire::decode_args(&spec.args, &types);
    let mut call = SurfaceCall::new(address, args);
    if let Some(operation_id) = &spec.operation_id {
        call = call.with_operation_id(operation_id.clone());
    }

    if let Some(claim) = &spec.claim {
        return put_declared(loaded, spec, claim);
    }

    let outcome = loaded
        .host
        .call_with_blob(&spec.connection, call, &spec.param, &spec.content, &spec.media)
        .map_err(host_fault)?;
    use liasse_surface::SurfaceOutcome as O;
    if matches!(outcome, O::Committed { .. } | O::Unchanged { .. }) {
        digests.insert(spec.param.clone(), BlobIntegrity::digest_hex(&spec.content));
    }
    Ok(observe_call(&outcome))
}

/// Verify a client-declared descriptor (a `claim`) against the streamed bytes
/// (§18.1/§18.2). A negative `$bytes` is a malformed descriptor value the
/// [`DeclaredDescriptor`] type cannot even carry, so it is rejected at the wire
/// boundary. A verifying descriptor that would still need binding into the
/// mutation is a precise seam (the surface admits only honest blob parameters).
fn put_declared<S: InstanceStore>(
    loaded: &mut Loaded<S>,
    spec: &BlobPutSpec,
    claim: &J,
) -> Result<Observation, AdapterError> {
    if let Some(bytes) = claim.get("$bytes").and_then(J::as_i64)
        && bytes < 0
    {
        // §18.1: `$bytes` is a non-negative integer; a negative claim is a
        // malformed descriptor, rejected before any transition.
        return Ok(Observation::outcome(Outcome::Rejected));
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
    match loaded.host.blob_put_declared(&spec.param, &declared, &spec.content).map_err(component_fault)? {
        BlobPutOutcome::Rejected(_) => Ok(Observation::outcome(Outcome::Rejected)),
        BlobPutOutcome::Committed { .. } => Err(AdapterError::unsupported(
            "a client-declared blob descriptor that verifies must be bound into the mutation call, \
             but the surface admits only the honest `call_with_blob` blob parameter (no \
             declared-descriptor call binding is exposed)",
        )),
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
        if let Some(connector) = loaded.host.connector_mut(field, &target).map_err(component_fault)? {
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

/// Map a host-component driver error (an unregistered component) to a skip.
fn component_fault(error: liasse_surface::HostComponentError) -> AdapterError {
    AdapterError::Host(error.to_string())
}
