//! §18.3 connector-registry ingress/serve integration.
//!
//! These tests intentionally exercise package-declared `$blob_storage` through
//! the engine-owned host registry. No manually assembled `BlobEngine`/`BlobHost`
//! is involved: the definition is the placement source of truth.

use liasse_host::sim::SimConnector;
use liasse_host::{BlobConnector, Capability, ConnectorCapabilities};
use liasse_ident::InstanceId;
use liasse_runtime::{
    CallOutcome, CallRequest, DeclaredDescriptor, Engine, EngineError, FetchError, FixedGenerators,
    Precision, Registry, RejectionReason, Value,
};
use liasse_store::MemoryStore;
use liasse_value::{BlobDescriptor, Text};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const NOW: i128 = 1_700_000_000_000_000;
const PACKAGE: &str = r#"{
  "$liasse": 1
  "$app": "t.blob.registry@1.0.0"
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
        "$media": ["application/octet-stream"]
      }
    }
    "$mut": {
      "add": ".docs + { id: @id, file: @file }"
      "reject({ file: blob })": ["assert(false)"]
    }
  }
  "$data": {
    "stores": {
      "primary": { "connector": "memory" }
    }
  }
}"#;

fn generator() -> FixedGenerators {
    FixedGenerators::new(NOW, Precision::Micros)
}

fn registry() -> Registry {
    let capabilities = ConnectorCapabilities::new([
        Capability::StreamUpload,
        Capability::StreamDownload,
        Capability::Checksum,
        Capability::Delete,
        Capability::PhysicalUsage,
    ]);
    let mut registry = Registry::new();
    registry.register_connector(
        "memory",
        Box::new(SimConnector::new(capabilities)) as Box<dyn BlobConnector>,
    );
    registry
}

fn engine(instance: &str) -> TestResult<Engine<MemoryStore>> {
    engine_from(instance, PACKAGE)
}

fn engine_from(instance: &str, package: &str) -> TestResult<Engine<MemoryStore>> {
    let store = MemoryStore::new(InstanceId::new(instance));
    Ok(Engine::load_with_hosts(
        store,
        package,
        &mut generator(),
        registry(),
    )?)
}

fn declared(bytes: &[u8], name: &str) -> DeclaredDescriptor {
    DeclaredDescriptor {
        sha512: liasse_host::BlobIntegrity::digest_hex(bytes),
        bytes: bytes.len() as u64,
        media: "application/octet-stream".to_owned(),
        name: Some(name.to_owned()),
    }
}

fn add(
    engine: &mut Engine<MemoryStore>,
    id: &str,
    bytes: &[u8],
    name: &str,
) -> TestResult<(CallOutcome, BlobDescriptor)> {
    let ingress = engine
        .stage_blob("add", "file", &declared(bytes, name), bytes)
        .map_err(|rejection| std::io::Error::other(rejection.message().to_owned()))?;
    let descriptor = ingress.descriptor().clone();
    let request = CallRequest::new("add")
        .arg("id", Value::Text(Text::new(id)))
        .arg("file", Value::Blob(Box::new(descriptor.clone())));
    let outcome = engine.call_with_blob(&request, ingress, &mut generator())?;
    Ok((outcome, descriptor))
}

/// A package declaration, registered connector, mutation ingress, and serve read
/// form one end-to-end path; served bytes are the exact admitted bytes.
#[test]
fn package_declared_ingress_round_trips_through_serve_store() -> TestResult {
    let mut engine = engine("blob-round-trip")?;
    let bytes = b"\0package-declared blob bytes\xff";
    let (outcome, descriptor) = add(&mut engine, "d1", bytes, "payload.bin")?;
    assert!(matches!(outcome, CallOutcome::Committed { .. }));

    let fetched = engine.fetch_blob(&descriptor)?;
    assert_eq!(fetched.bytes(), bytes);
    assert_eq!(
        fetched
            .holders()
            .first()
            .map(liasse_runtime::StoreId::as_str),
        Some("primary")
    );
    Ok(())
}

/// Content identity, not descriptor metadata or admission count, keys physical
/// storage: two occurrences with identical bytes remain one connector object.
#[test]
fn identical_content_is_physically_deduplicated() -> TestResult {
    let mut engine = engine("blob-dedup")?;
    let bytes = b"one content identity";
    assert!(matches!(
        add(&mut engine, "d1", bytes, "first.bin")?.0,
        CallOutcome::Committed { .. }
    ));
    assert!(matches!(
        add(&mut engine, "d2", bytes, "second.bin")?.0,
        CallOutcome::Committed { .. }
    ));

    let usage = engine
        .blob_connector("memory")
        .ok_or_else(|| std::io::Error::other("connector was not retained by the engine registry"))?
        .observe_usage()?;
    assert_eq!(usage.object_count, 1);
    assert_eq!(usage.physical_bytes, bytes.len() as u64);
    Ok(())
}

/// Supplying the host registry is an explicit activation boundary. A seeded
/// placement-reachable store row naming an absent connector fails there loudly.
#[test]
fn placement_reachable_unregistered_connector_fails_activation() -> TestResult {
    let store = MemoryStore::new(InstanceId::new("blob-missing-connector"));
    let result = Engine::load_with_hosts(store, PACKAGE, &mut generator(), Registry::new());
    let message = match result {
        Err(EngineError::Requirement(message)) => message,
        Err(other) => {
            return Err(std::io::Error::other(format!(
                "expected connector requirement failure, got {other}"
            ))
            .into());
        }
        Ok(_) => {
            return Err(std::io::Error::other(
                "package activated with an unregistered placement connector",
            )
            .into());
        }
    };
    assert!(
        message.contains("memory"),
        "diagnostic names missing connector: {message}"
    );
    Ok(())
}

/// The upload transport may stage bytes before admission, but a mutation
/// rejection commits neither a physical object nor a serveable logical blob.
#[test]
fn rejected_mutation_does_not_commit_or_serve_staged_blob() -> TestResult {
    let mut engine = engine("blob-reject")?;
    let bytes = b"must remain uncommitted";
    let ingress = engine
        .stage_blob("reject", "file", &declared(bytes, "rejected.bin"), bytes)
        .map_err(|rejection| std::io::Error::other(rejection.message().to_owned()))?;
    let descriptor = ingress.descriptor().clone();
    let request = CallRequest::new("reject").arg("file", Value::Blob(Box::new(descriptor.clone())));
    let outcome = engine.call_with_blob(&request, ingress, &mut generator())?;
    let CallOutcome::Rejected(rejection) = outcome else {
        return Err(std::io::Error::other("assertion mutation committed").into());
    };
    assert_eq!(rejection.reason(), RejectionReason::Assertion);

    let usage = engine
        .blob_connector("memory")
        .ok_or_else(|| std::io::Error::other("connector was not retained by the engine registry"))?
        .observe_usage()?;
    assert_eq!(usage.object_count, 0);
    assert_eq!(engine.fetch_blob(&descriptor), Err(FetchError::Unknown));
    Ok(())
}

/// Finding 1: a bare `Engine::call` carrying a blob descriptor argument WITHOUT a
/// staged ingress must be refused loudly before any state change — never
/// committed as an unbacked, unfetchable blob field. Persist-or-refuse holds at
/// the lowest admission entry point.
#[test]
fn bare_call_with_unbacked_blob_descriptor_is_refused() -> TestResult {
    let mut engine = engine("blob-unbacked")?;
    let bytes = b"descriptor without managed admission";
    // Stage to obtain the verified descriptor, then DISCARD the owned ingress.
    let descriptor = engine
        .stage_blob("add", "file", &declared(bytes, "orphan.bin"), bytes)
        .map_err(|rejection| std::io::Error::other(rejection.message().to_owned()))?
        .descriptor()
        .clone();
    let head_before = engine.head()?;

    let request = CallRequest::new("add")
        .arg("id", Value::Text(Text::new("d1")))
        .arg("file", Value::Blob(Box::new(descriptor.clone())));
    let outcome = engine.call(&request, &mut generator())?;

    let CallOutcome::Rejected(rejection) = outcome else {
        return Err(
            std::io::Error::other("unbacked blob descriptor was admitted through `call`").into(),
        );
    };
    assert_eq!(rejection.reason(), RejectionReason::Malformed);
    assert_eq!(
        engine.head()?,
        head_before,
        "no commit for a refused blob admission"
    );
    let usage = engine
        .blob_connector("memory")
        .ok_or_else(|| std::io::Error::other("connector missing"))?
        .observe_usage()?;
    assert_eq!(
        usage.object_count, 0,
        "no physical object for a refused admission"
    );
    assert_eq!(engine.fetch_blob(&descriptor), Err(FetchError::Unknown));
    Ok(())
}

const ANY_PACKAGE: &str = r#"{
  "$liasse": 1
  "$app": "t.blob.any@1.0.0"
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
        "$in": { "$any": ["/stores[:s | s.id == 'missing']", "/stores['primary']"] }
      }
      "id": "text"
      "file": {
        "$type": "blob"
        "$max_bytes": "1024"
        "$media": ["application/octet-stream"]
      }
    }
    "$mut": {
      "add": ".docs + { id: @id, file: @file }"
    }
  }
  "$data": {
    "stores": {
      "primary": { "connector": "memory" }
    }
  }
}"#;

/// Finding 2: an empty `$any` alternative is not a fulfillable zero-copy write
/// plan. The runtime must skip the empty first branch and land the copy in the
/// later fulfillable `primary` branch — never report success with no landed copy.
#[test]
fn empty_any_branch_is_skipped_and_bytes_land() -> TestResult {
    let mut engine = engine_from("blob-empty-any", ANY_PACKAGE)?;
    let bytes = b"must land somewhere";
    let ingress = engine
        .stage_blob("add", "file", &declared(bytes, "doc.bin"), bytes)
        .map_err(|rejection| std::io::Error::other(rejection.message().to_owned()))?;
    let descriptor = ingress.descriptor().clone();
    let request = CallRequest::new("add")
        .arg("id", Value::Text(Text::new("d1")))
        .arg("file", Value::Blob(Box::new(descriptor.clone())));
    let outcome = engine.call_with_blob(&request, ingress, &mut generator())?;
    assert!(
        matches!(outcome, CallOutcome::Committed { .. }),
        "expected a committed write, got {outcome:?}"
    );

    let usage = engine
        .blob_connector("memory")
        .ok_or_else(|| std::io::Error::other("connector missing"))?
        .observe_usage()?;
    assert_eq!(usage.object_count, 1, "exactly one landed copy (not zero)");
    let fetched = engine.fetch_blob(&descriptor)?;
    assert_eq!(fetched.bytes(), bytes);
    assert_eq!(
        fetched
            .holders()
            .first()
            .map(liasse_runtime::StoreId::as_str),
        Some("primary"),
        "the copy landed in the later fulfillable branch"
    );
    Ok(())
}

const ALIAS_PACKAGE: &str = r#"{
  "$liasse": 1
  "$app": "t.blob.alias@1.0.0"
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
        "$media": ["application/octet-stream"]
      }
    }
    "$mut": {
      "add": ".docs + { id: @id, file: @payload }"
    }
  }
  "$data": {
    "stores": {
      "primary": { "connector": "memory" }
    }
  }
}"#;

/// Finding 3: a blob parameter need not share its destination field's spelling.
/// `@payload` feeds the `file` field, so staging under the parameter name must
/// use `file`'s accepted type and placement — and the aliased upload round-trips.
#[test]
fn aliased_blob_parameter_routes_to_its_field() -> TestResult {
    let mut engine = engine_from("blob-alias", ALIAS_PACKAGE)?;
    let bytes = b"aliased parameter";
    let ingress = engine
        .stage_blob("add", "payload", &declared(bytes, "doc.bin"), bytes)
        .map_err(|rejection| std::io::Error::other(rejection.message().to_owned()))?;
    let descriptor = ingress.descriptor().clone();
    let request = CallRequest::new("add")
        .arg("id", Value::Text(Text::new("d1")))
        .arg("payload", Value::Blob(Box::new(descriptor.clone())));
    let outcome = engine.call_with_blob(&request, ingress, &mut generator())?;
    assert!(
        matches!(outcome, CallOutcome::Committed { .. }),
        "aliased blob upload committed"
    );

    let fetched = engine.fetch_blob(&descriptor)?;
    assert_eq!(fetched.bytes(), bytes);
    Ok(())
}

const RELATIVE_PACKAGE: &str = r#"{
  "$liasse": 1
  "$app": "t.blob.relative@1.0.0"
  "$model": {
    "owners": {
      "$key": "id"
      "id": "text"
      "stores": {
        "$key": "id"
        "id": "text"
        "connector": "text"
        "enabled": "bool = true"
      }
      "docs": {
        "$key": "id"
        "$blob_storage": { "$in": "^.stores['primary']" }
        "id": "text"
        "file": {
          "$type": "blob"
          "$max_bytes": "1024"
          "$media": ["application/octet-stream"]
        }
      }
    }
  }
  "$data": {
    "owners": {
      "o1": { "stores": { "primary": { "connector": "memory" } } }
    }
  }
}"#;

/// Finding 4: occurrence-relative placement views are an unsupported §18.4 subset.
/// They must be refused LOUDLY at load (an honest `EngineError::Unsupported`
/// naming the view), never silently mis-resolved — documented as loud-deferred.
#[test]
fn relative_placement_view_is_loudly_unsupported() -> TestResult {
    let store = MemoryStore::new(InstanceId::new("blob-relative"));
    let result = Engine::load_with_hosts(store, RELATIVE_PACKAGE, &mut generator(), registry());
    match result {
        Err(EngineError::Unsupported(message)) => {
            assert!(
                message.contains("^.stores['primary']"),
                "the unsupported diagnostic names the relative view: {message}"
            );
            Ok(())
        }
        Err(other) => Err(std::io::Error::other(format!(
            "expected a loud Unsupported refusal, got {other}"
        ))
        .into()),
        Ok(_) => Err(std::io::Error::other(
            "a relative placement view loaded instead of a loud unsupported refusal",
        )
        .into()),
    }
}
