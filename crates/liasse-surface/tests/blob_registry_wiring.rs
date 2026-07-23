//! Surface ingress/serve wiring over package-declared `$blob_storage`.

use std::collections::BTreeMap;

use liasse_host::sim::SimConnector;
use liasse_host::{BlobConnector, BlobIntegrity, Capability, ConnectorCapabilities, Registry};
use liasse_ident::InstanceId;
use liasse_store::MemoryStore;
use liasse_surface::{
    BlobGetOutcome, CallBinding, DeclaredDescriptor, Engine, Entropy, Precision, SurfaceAddress,
    SurfaceBinding, SurfaceCall, SurfaceHost, SurfaceOutcome, SurfaceRouterBuilder, VirtualClock,
};
use liasse_value::{Text, Value};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const NOW: i128 = 1_700_000_000_000_000;
const PACKAGE: &str = r#"{
  "$liasse": 1
  "$app": "t.blob.surface@1.0.0"
  "$model": {
    "stores": {
      "$key": "id"
      "id": "text"
      "connector": "text"
      "enabled": "bool = true"
    }
    "docs": {
      "$key": "id"
      "$blob_storage": {
        "$in": "/stores['primary']"
        "$serve": "/stores['primary']"
      }
      "id": "text"
      "file": {
        "$type": "blob"
        "$max_bytes": "1024"
        "$media": ["text/plain"]
      }
    }
    "$mut": {
      "add": ".docs + { id: @id, file: @file }"
    }
    "$public": {
      "docs": { "$mut": { "add": ".add" } }
    }
  }
  "$data": {
    "stores": { "primary": { "connector": "memory" } }
  }
}"#;

fn host() -> TestResult<SurfaceHost<MemoryStore>> {
    let capabilities = ConnectorCapabilities::new([
        Capability::StreamUpload,
        Capability::StreamDownload,
        Capability::Checksum,
        Capability::Delete,
    ]);
    let mut registry = Registry::new();
    registry.register_connector(
        "memory",
        Box::new(SimConnector::new(capabilities)) as Box<dyn BlobConnector>,
    );
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = Engine::load_with_hosts(
        MemoryStore::new(InstanceId::new("blob-surface")),
        PACKAGE,
        &mut clock,
        registry,
    )?;
    let docs = SurfaceBinding::new().with_call(
        "add",
        CallBinding::root("add", ["id".to_owned()]).with_blobs(["file".to_owned()]),
    );
    let router = SurfaceRouterBuilder::new()
        .public_surface("docs", docs)
        .build(engine.model())?;
    Ok(SurfaceHost::new(engine, router, clock).with_entropy(Entropy::seeded(7)))
}

#[test]
fn surface_blob_put_uses_compiled_placement_and_engine_registry() -> TestResult {
    let bytes = b"surface-managed bytes";
    let declared = DeclaredDescriptor {
        sha512: BlobIntegrity::digest_hex(bytes),
        bytes: bytes.len() as u64,
        media: "text/plain".to_owned(),
        name: Some("note.txt".to_owned()),
    };
    let mut host = host()?;
    host.connect("c1")?;
    let call = SurfaceCall::new(
        SurfaceAddress::parse("public.docs.add")?,
        BTreeMap::from([("id".to_owned(), Value::Text(Text::new("d1")))]),
    );
    let outcome = host.call_with_declared_blob("c1", call, "file", &declared, bytes)?;
    assert!(matches!(outcome, SurfaceOutcome::Committed { .. }));
    assert_eq!(
        host.blob_get("file", &declared.sha512, true)?,
        BlobGetOutcome::Delivered {
            bytes: bytes.to_vec(),
            holders: vec![liasse_surface::StoreId::new("primary")],
        }
    );
    Ok(())
}
