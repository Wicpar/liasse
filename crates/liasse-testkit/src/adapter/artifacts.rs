//! Driving the `.liasse` artifact op families (§4.1, §4.2, §19.5, Annex D.5).
//!
//! Four steps operate over the adapter's shared byte table
//! ([`artifacts`](super::ScenarioAdapter::artifacts)), the same table
//! `export`/`import`/`restore` use:
//!
//! - `build_artifact` assembles the case package (canonical `liasse.json`, an
//!   empty state/history section, and the `files{}` overlay) into a `.liasse`
//!   via [`ArtifactBuilder`], computing every manifest checksum over the **final**
//!   bytes and performing **no** validation (the validation under test is at load).
//! - `repack_artifact` applies in-place byte surgery to one label
//!   (set/add/remove/merge_json/repack/rehash), never touching the manifest
//!   checksums unless `rehash` is given — so a case can isolate the Annex D.5
//!   container-integrity layer from the content rules.
//! - `tamper_artifact` derives a **new** label from a source by the §19 op
//!   vocabulary (duplicate_entry/edit_json/fix_checksums), leaving the source
//!   untouched, so a later `restore` re-verifies the tampered bytes.
//! - `load_artifact` runs the §9.2 create lifecycle: open-and-verify the archive
//!   (§19.8/D.5), reject a non-strict-canonical `liasse.json` (duplicate member),
//!   verify each `$resources` `$sha256` against the served bytes (§4.1), then load
//!   the definition. `ok` = committed, `invalid` = rejected.

use liasse_artifact::{Artifact, ArtifactBuilder};
use liasse_ident::{Digest, HistoryPoint, InstanceId, LineageId, PointId};
use liasse_runtime::{Engine, Precision};
use liasse_store::{InstanceStore, MemoryStore};
use liasse_surface::VirtualClock as SurfaceClock;
use serde_json::Value as J;

use crate::contract::Observation;
use crate::outcome::Outcome;
use crate::request::OpRequest;

use super::{AdapterError, EPOCH_MICROS};

/// The archive path of the manifest entry (§19.5).
const MANIFEST_JSON: &str = "manifest.json";

impl<S: InstanceStore> super::ScenarioAdapter<S> {
    /// §4.2 `build_artifact`: assemble the case package plus its `files{}` overlay
    /// into a verifiable `.liasse` and bind it under the step's `as` label.
    pub(super) fn drive_build_artifact(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        let target = &request.target;
        let Some(label) = target.get("as").and_then(J::as_str) else {
            return Err(AdapterError::unsupported("`build_artifact` step carries no `as` label"));
        };
        let label = label.to_owned();
        let package = self.load_ctx.package.clone();
        let definition =
            serde_json::to_vec(&package).map_err(|err| AdapterError::Host(err.to_string()))?;

        let mut builder = ArtifactBuilder::new(
            InstanceId::new(format!("{}#artifact", self.load_ctx.instance.as_str())),
            HistoryPoint::new(LineageId::new("genesis"), PointId::new("genesis")),
            definition,
            Vec::new(),
            Vec::new(),
        );
        // §4.1 `$resources` / §19.5 extra sections: each `files{}` entry becomes an
        // archive entry whose manifest checksum is over its final bytes. A resource
        // path carries its declared `$media`; anything else a generic media type.
        if let Some(files) = target.get("files").and_then(J::as_object) {
            for (path, content) in files {
                let bytes = content.as_str().unwrap_or_default().as_bytes().to_vec();
                let media = resource_media(&package, path).unwrap_or("application/octet-stream");
                builder.section(path.clone(), media, bytes);
            }
        }
        let bytes = builder.build().map_err(|err| AdapterError::Host(err.to_string()))?;
        self.artifacts.insert(label, bytes);
        Ok(Observation::ok(None))
    }

    /// §4 `repack_artifact`: in-place byte surgery on one artifact label.
    pub(super) fn drive_repack_artifact(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        let target = &request.target;
        let Some(label) = target.get("artifact").and_then(J::as_str) else {
            return Err(AdapterError::unsupported("`repack_artifact` step carries no `artifact` label"));
        };
        let label = label.to_owned();
        let bytes = self
            .artifacts
            .get(&label)
            .ok_or_else(|| AdapterError::unsupported(format!("`repack_artifact` names no artifact `{label}` in scope")))?;
        let mut entries = super::rawzip::read_ordered(bytes).map_err(AdapterError::Host)?;

        if let Some(remove) = target.get("remove").and_then(J::as_array) {
            for path in remove.iter().filter_map(J::as_str) {
                entries.retain(|(name, _)| name != path);
            }
        }
        if let Some(set) = target.get("set").and_then(J::as_object) {
            for (path, content) in set {
                set_entry(&mut entries, path, content.as_str().unwrap_or_default().as_bytes().to_vec());
            }
        }
        if let Some(add) = target.get("add").and_then(J::as_object) {
            for (path, content) in add {
                entries.push((path.clone(), content.as_str().unwrap_or_default().as_bytes().to_vec()));
            }
        }
        if let Some(merge) = target.get("merge_json").and_then(J::as_object) {
            merge_json(&mut entries, merge)?;
        }
        if target.get("rehash").and_then(J::as_bool) == Some(true) {
            rehash(&mut entries)?;
        }

        let repacked = super::rawzip::write_ordered(&entries);
        self.artifacts.insert(label, repacked);
        Ok(Observation::ok(None))
    }

    /// §19 `tamper_artifact`: derive a new label by applying the byte-surgery op
    /// list to a copy of the source, leaving the source untouched.
    pub(super) fn drive_tamper_artifact(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        let target = &request.target;
        let Some(from) = target.get("from").and_then(J::as_str) else {
            return Err(AdapterError::unsupported("`tamper_artifact` step carries no `from` label"));
        };
        let Some(label) = target.get("as").and_then(J::as_str) else {
            return Err(AdapterError::unsupported("`tamper_artifact` step carries no `as` label"));
        };
        let (from, label) = (from.to_owned(), label.to_owned());
        let bytes = self
            .artifacts
            .get(&from)
            .ok_or_else(|| AdapterError::unsupported(format!("`tamper_artifact` names no artifact `{from}` in scope")))?;
        let mut entries = super::rawzip::read_ordered(bytes).map_err(AdapterError::Host)?;

        for op in target.get("ops").and_then(J::as_array).into_iter().flatten() {
            apply_tamper_op(&mut entries, op)?;
        }
        let tampered = super::rawzip::write_ordered(&entries);
        self.artifacts.insert(label, tampered);
        Ok(Observation::ok(None))
    }

    /// §19.5 `inspect_artifact`: open-and-verify the artifact and return its
    /// `manifest.json` as the observed value, so the step's expectation can bind
    /// and compare recorded members (e.g. the §D.4 `definition.identity`).
    pub(super) fn drive_inspect_artifact(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        let target = &request.target;
        let Some(label) = target.get("artifact").and_then(J::as_str) else {
            return Err(AdapterError::unsupported("`inspect_artifact` step carries no `artifact` label"));
        };
        let bytes = self
            .artifacts
            .get(label)
            .ok_or_else(|| AdapterError::unsupported(format!("`inspect_artifact` names no artifact `{label}` in scope")))?;
        let Ok(artifact) = Artifact::open(bytes) else {
            return Ok(Observation::outcome(Outcome::Invalid));
        };
        let manifest = artifact.entry(MANIFEST_JSON).unwrap_or_default();
        let value: J = serde_json::from_slice(manifest).map_err(|err| AdapterError::Host(err.to_string()))?;
        Ok(Observation::ok(Some(value)))
    }

    /// §9.2 `load_artifact`: the host `create` lifecycle over a built artifact —
    /// open-and-verify the container (§19.8/D.5), reject a non-canonical
    /// `liasse.json`, verify each declared resource digest (§4.1), then load.
    pub(super) fn drive_load_artifact(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        let target = &request.target;
        let Some(label) = target.get("from").and_then(J::as_str) else {
            return Err(AdapterError::unsupported("`load_artifact` step carries no `from` label"));
        };
        let bytes = self
            .artifacts
            .get(label)
            .ok_or_else(|| AdapterError::unsupported(format!("`load_artifact` names no artifact `{label}` in scope")))?
            .clone();
        Ok(Observation::outcome(self.load_artifact_outcome(&bytes)))
    }

    /// The §9.2 create outcome for artifact `bytes`: `Ok` on activation, `Invalid`
    /// at the first verification failure.
    fn load_artifact_outcome(&self, bytes: &[u8]) -> Outcome {
        // §19.8/D.5 container verification: mimetype, closed manifest, exactly-once
        // entries, and every recorded checksum.
        let Ok(artifact) = Artifact::open(bytes) else {
            return Outcome::Invalid;
        };
        let definition = artifact.liasse_json();
        // §4.1/D.4: `liasse.json` is strict canonical JSON, which has unique member
        // names — a duplicate-member document is rejected rather than resolved to
        // whichever occurrence a lenient parser happens to keep.
        if !has_unique_members(definition) {
            return Outcome::Invalid;
        }
        // §4.1: every declared `$resources` digest is verified against the served
        // bytes before activation.
        if !resources_verify(&artifact, definition) {
            return Outcome::Invalid;
        }
        // §9.2 activation: the definition must compile and its genesis seed admit.
        let Ok(text) = std::str::from_utf8(definition) else {
            return Outcome::Invalid;
        };
        let store = MemoryStore::new(InstanceId::new(format!("{}#load", self.load_ctx.instance.as_str())));
        let mut clock = SurfaceClock::new(EPOCH_MICROS, Precision::Micros);
        match Engine::load(store, text, &mut clock) {
            Ok(_) => Outcome::Ok,
            Err(_) => Outcome::Invalid,
        }
    }
}

/// Replace the first entry named `path` with `bytes`, appending it when absent.
fn set_entry(entries: &mut Vec<(String, Vec<u8>)>, path: &str, bytes: Vec<u8>) {
    match entries.iter_mut().find(|(name, _)| name == path) {
        Some((_, data)) => *data = bytes,
        None => entries.push((path.to_owned(), bytes)),
    }
}

/// Apply one `merge_json` directive: parse the named entry, set its top-level
/// members, and re-serialize (§4 repack `merge_json`).
fn merge_json(entries: &mut [(String, Vec<u8>)], merge: &serde_json::Map<String, J>) -> Result<(), AdapterError> {
    let Some(path) = merge.get("path").and_then(J::as_str) else {
        return Err(AdapterError::unsupported("`merge_json` carries no `path`"));
    };
    let set = merge.get("set").and_then(J::as_object);
    let Some((_, data)) = entries.iter_mut().find(|(name, _)| name == path) else {
        return Err(AdapterError::unsupported(format!("`merge_json` names absent entry `{path}`")));
    };
    let mut value: J = serde_json::from_slice(data).map_err(|err| AdapterError::Host(err.to_string()))?;
    if let (Some(object), Some(set)) = (value.as_object_mut(), set) {
        for (member, member_value) in set {
            object.insert(member.clone(), member_value.clone());
        }
    }
    *data = serde_json::to_vec(&value).map_err(|err| AdapterError::Host(err.to_string()))?;
    Ok(())
}

/// Recompute every manifest byte checksum (`entries`, `state`, `history`,
/// `included_modules`) over the final entry bytes, leaving `definition.identity`
/// (the §D.4 semantic identity) untouched — the repack `rehash` / tamper
/// `fix_checksums` operation.
fn rehash(entries: &mut [(String, Vec<u8>)]) -> Result<(), AdapterError> {
    let by_name: std::collections::BTreeMap<&str, &[u8]> =
        entries.iter().map(|(name, data)| (name.as_str(), data.as_slice())).collect();
    let digest_of = |path: &str| by_name.get(path).map(|bytes| Digest::of_bytes(bytes).to_canonical_text());

    let Some(manifest_bytes) = by_name.get(MANIFEST_JSON).copied() else {
        return Err(AdapterError::unsupported("`rehash` finds no `manifest.json` entry"));
    };
    let mut manifest: J =
        serde_json::from_slice(manifest_bytes).map_err(|err| AdapterError::Host(err.to_string()))?;

    if let Some(map) = manifest.get_mut("entries").and_then(J::as_object_mut) {
        for (path, entry) in map.iter_mut() {
            if let (Some(sha), Some(object)) = (digest_of(path), entry.as_object_mut()) {
                object.insert("sha256".to_owned(), J::String(sha));
            }
        }
    }
    for section in ["state", "history"] {
        if let Some(object) = manifest.get_mut(section).and_then(J::as_object_mut)
            && let Some(path) = object.get("path").and_then(J::as_str).map(ToOwned::to_owned)
            && let Some(sha) = digest_of(&path)
        {
            object.insert("sha256".to_owned(), J::String(sha));
        }
    }
    if let Some(map) = manifest.get_mut("included_modules").and_then(J::as_object_mut) {
        for module in map.values_mut() {
            if let Some(object) = module.as_object_mut()
                && let Some(path) = object.get("artifact").and_then(J::as_str).map(ToOwned::to_owned)
                && let Some(sha) = digest_of(&path)
            {
                object.insert("sha256".to_owned(), J::String(sha));
            }
        }
    }

    let rehashed = serde_json::to_vec(&manifest).map_err(|err| AdapterError::Host(err.to_string()))?;
    if let Some((_, data)) = entries.iter_mut().find(|(name, _)| name == MANIFEST_JSON) {
        *data = rehashed;
    }
    Ok(())
}

/// Apply one §19 `tamper_artifact` op. The ops the annex-d archive-integrity
/// corpus exercises are supported; a broader §19 op is a precise skip.
fn apply_tamper_op(entries: &mut Vec<(String, Vec<u8>)>, op: &J) -> Result<(), AdapterError> {
    let Some((name, body)) = op.as_object().and_then(|map| map.iter().next()) else {
        return Err(AdapterError::unsupported("malformed `tamper_artifact` op"));
    };
    match name.as_str() {
        // Smuggle a second entry with identical bytes to the referenced one, so
        // every checksum still matches yet the entry exists twice (§D.5).
        "duplicate_entry" => {
            let Some(path) = body.get("path").and_then(J::as_str) else {
                return Err(AdapterError::unsupported("`duplicate_entry` carries no `path`"));
            };
            let Some((_, data)) = entries.iter().find(|(n, _)| n == path) else {
                return Err(AdapterError::unsupported(format!("`duplicate_entry` names absent entry `{path}`")));
            };
            let copy = (path.to_owned(), data.clone());
            entries.push(copy);
            Ok(())
        }
        // Edit an entry's JSON at a pointer (leaving the manifest checksum stale
        // unless a later `fix_checksums` op recomputes it).
        "edit_json" => {
            let Some(path) = body.get("path").and_then(J::as_str) else {
                return Err(AdapterError::unsupported("`edit_json` carries no `path`"));
            };
            let Some(pointer) = body.get("pointer").and_then(J::as_str) else {
                return Err(AdapterError::unsupported("`edit_json` carries no `pointer`"));
            };
            let new = body.get("value").cloned().unwrap_or(J::Null);
            let Some((_, data)) = entries.iter_mut().find(|(n, _)| n == path) else {
                return Err(AdapterError::unsupported(format!("`edit_json` names absent entry `{path}`")));
            };
            let mut value: J = serde_json::from_slice(data).map_err(|err| AdapterError::Host(err.to_string()))?;
            let Some(slot) = value.pointer_mut(pointer) else {
                return Err(AdapterError::unsupported(format!("`edit_json` pointer `{pointer}` does not resolve")));
            };
            *slot = new;
            *data = serde_json::to_vec(&value).map_err(|err| AdapterError::Host(err.to_string()))?;
            Ok(())
        }
        // Recompute every byte checksum so tampered bytes are self-consistent.
        "fix_checksums" => rehash(entries),
        other => Err(AdapterError::unsupported(format!(
            "`tamper_artifact` op `{other}` is not driven this phase (the annex-d archive-integrity \
             ops duplicate_entry/edit_json/fix_checksums are)"
        ))),
    }
}

/// The declared `$media` of the `$resources` descriptor whose `$path` is `path`,
/// for the manifest entry's recorded media type.
fn resource_media<'a>(package: &'a J, path: &str) -> Option<&'a str> {
    package
        .get("$resources")
        .and_then(J::as_object)?
        .values()
        .find(|descriptor| descriptor.get("$path").and_then(J::as_str) == Some(path))
        .and_then(|descriptor| descriptor.get("$media").and_then(J::as_str))
}

/// Whether every declared `$resources` digest matches the served entry bytes
/// (§4.1). A missing entry or a byte mismatch fails verification.
fn resources_verify(artifact: &Artifact, definition: &[u8]) -> bool {
    let Ok(value): Result<J, _> = serde_json::from_slice(definition) else {
        return false;
    };
    let Some(resources) = value.get("$resources").and_then(J::as_object) else {
        return true;
    };
    for descriptor in resources.values() {
        let (Some(path), Some(declared)) = (
            descriptor.get("$path").and_then(J::as_str),
            descriptor.get("$sha256").and_then(J::as_str),
        ) else {
            continue;
        };
        let Some(entry) = artifact.entry(path) else {
            return false;
        };
        let actual = Digest::of_bytes(entry).to_canonical_text();
        let actual_hex = actual.strip_prefix("sha256:").unwrap_or(&actual);
        if !actual_hex.eq_ignore_ascii_case(declared.trim()) {
            return false;
        }
    }
    true
}

/// Whether a JSON document has unique object member names at every level — the
/// strict-canonical-JSON invariant a duplicate-member `liasse.json` breaks
/// (§4.1, D.4/D.5). `serde_json` silently keeps the last of a duplicate, so this
/// re-parses through a visitor that rejects a repeated key.
fn has_unique_members(bytes: &[u8]) -> bool {
    serde_json::from_slice::<UniqueKeys>(bytes).is_ok()
}

/// A `serde` shape whose deserialization fails on any repeated object member.
struct UniqueKeys;

impl<'de> serde::Deserialize<'de> for UniqueKeys {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(UniqueKeysVisitor)
    }
}

struct UniqueKeysVisitor;

impl<'de> serde::de::Visitor<'de> for UniqueKeysVisitor {
    type Value = UniqueKeys;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("any JSON value with unique object members")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<UniqueKeys, A::Error> {
        let mut seen = std::collections::HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(serde::de::Error::custom(format!("duplicate member `{key}`")));
            }
            map.next_value::<UniqueKeys>()?;
        }
        Ok(UniqueKeys)
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<UniqueKeys, A::Error> {
        while seq.next_element::<UniqueKeys>()?.is_some() {}
        Ok(UniqueKeys)
    }

    fn visit_bool<E>(self, _: bool) -> Result<UniqueKeys, E> {
        Ok(UniqueKeys)
    }
    fn visit_i64<E>(self, _: i64) -> Result<UniqueKeys, E> {
        Ok(UniqueKeys)
    }
    fn visit_u64<E>(self, _: u64) -> Result<UniqueKeys, E> {
        Ok(UniqueKeys)
    }
    fn visit_f64<E>(self, _: f64) -> Result<UniqueKeys, E> {
        Ok(UniqueKeys)
    }
    fn visit_str<E>(self, _: &str) -> Result<UniqueKeys, E> {
        Ok(UniqueKeys)
    }
    fn visit_none<E>(self) -> Result<UniqueKeys, E> {
        Ok(UniqueKeys)
    }
    fn visit_unit<E>(self) -> Result<UniqueKeys, E> {
        Ok(UniqueKeys)
    }
    fn visit_some<D: serde::Deserializer<'de>>(self, d: D) -> Result<UniqueKeys, D::Error> {
        d.deserialize_any(UniqueKeysVisitor)
    }
}
