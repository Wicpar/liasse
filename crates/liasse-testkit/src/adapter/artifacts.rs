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
            .ok_or_else(|| AdapterError::unsupported(format!("`tamper_artifact` names no artifact `{from}` in scope")))?
            .clone();
        let mut entries = super::rawzip::read_ordered(&bytes).map_err(AdapterError::Host)?;

        for op in target.get("ops").and_then(J::as_array).into_iter().flatten() {
            apply_tamper_op(&mut entries, op, &self.artifacts)?;
        }
        let tampered = super::rawzip::write_ordered(&entries);
        self.artifacts.insert(label, tampered);
        Ok(Observation::ok(None))
    }

    /// §19.5 `extract_artifact`: pull one archive entry that is itself a nested
    /// `.liasse` (every entry below `modules/` is a complete artifact) and bind its
    /// bytes under the step's `as` label. `entry` is a glob that must match exactly
    /// one entry.
    pub(super) fn drive_extract_artifact(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        let target = &request.target;
        let Some(from) = target.get("from").and_then(J::as_str) else {
            return Err(AdapterError::unsupported("`extract_artifact` step carries no `from` label"));
        };
        let Some(glob) = target.get("entry").and_then(J::as_str) else {
            return Err(AdapterError::unsupported("`extract_artifact` step carries no `entry` glob"));
        };
        let Some(label) = target.get("as").and_then(J::as_str) else {
            return Err(AdapterError::unsupported("`extract_artifact` step carries no `as` label"));
        };
        let label = label.to_owned();
        let bytes = self
            .artifacts
            .get(from)
            .ok_or_else(|| AdapterError::unsupported(format!("`extract_artifact` names no artifact `{from}` in scope")))?;
        let entries = super::rawzip::read_ordered(bytes).map_err(AdapterError::Host)?;
        let mut matched = entries.iter().filter(|(name, _)| glob_match(glob, name));
        match (matched.next(), matched.next()) {
            (Some((_, data)), None) => {
                let data = data.clone();
                self.artifacts.insert(label, data);
                Ok(Observation::ok(None))
            }
            (None, _) => Err(AdapterError::unsupported(format!(
                "`extract_artifact` glob `{glob}` matched no archive entry"
            ))),
            (Some(_), Some(_)) => Err(AdapterError::unsupported(format!(
                "`extract_artifact` glob `{glob}` matched several archive entries (must match exactly one)"
            ))),
        }
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

/// Apply one §19 `tamper_artifact` op. The byte- and JSON-surgery ops the §19 and
/// annex-d integrity corpus exercises are driven; a CBOR-state edit is a precise
/// skip (the state section carries the runtime's keyed-collection codec, so its
/// logical pointer needs schema-owned key resolution beyond byte surgery).
fn apply_tamper_op(
    entries: &mut Vec<(String, Vec<u8>)>,
    op: &J,
    artifacts: &std::collections::BTreeMap<String, Vec<u8>>,
) -> Result<(), AdapterError> {
    let Some((name, body)) = op.as_object().and_then(|map| map.iter().next()) else {
        return Err(AdapterError::unsupported("malformed `tamper_artifact` op"));
    };
    match name.as_str() {
        // Smuggle a second entry with identical bytes to the referenced one, so
        // every checksum still matches yet the entry exists twice (§D.5).
        "duplicate_entry" => {
            let path = entry_path(body, "duplicate_entry")?;
            let Some((_, data)) = entries.iter().find(|(n, _)| n == path) else {
                return Err(AdapterError::unsupported(format!("`duplicate_entry` names absent entry `{path}`")));
            };
            let copy = (path.to_owned(), data.clone());
            entries.push(copy);
            Ok(())
        }
        // Flip the last byte of an entry, so its checksum no longer matches (§D.5).
        "corrupt_entry" => {
            let path = entry_path(body, "corrupt_entry")?;
            let Some((_, data)) = entries.iter_mut().find(|(n, _)| n == path) else {
                return Err(AdapterError::unsupported(format!("`corrupt_entry` names absent entry `{path}`")));
            };
            match data.last_mut() {
                Some(last) => *last ^= 0x01,
                None => data.push(0x01),
            }
            Ok(())
        }
        // Replace an entry's bytes with UTF-8 text.
        "set_entry" => {
            let path = entry_path(body, "set_entry")?;
            let text = body.get("text").and_then(J::as_str).unwrap_or_default();
            set_entry(entries, path, text.as_bytes().to_vec());
            Ok(())
        }
        // Delete an archive entry, leaving the manifest untouched (a dangling
        // reference the verifier catches).
        "remove_entry" => {
            let path = entry_path(body, "remove_entry")?;
            entries.retain(|(n, _)| n != path);
            Ok(())
        }
        // Add a new archive entry with UTF-8 text content.
        "add_entry" => {
            let path = entry_path(body, "add_entry")?;
            let text = body.get("text").and_then(J::as_str).unwrap_or_default();
            entries.push((path.to_owned(), text.as_bytes().to_vec()));
            Ok(())
        }
        // Replace an entry with the same-named entry's bytes from another artifact.
        "copy_entry_from" => {
            let path = entry_path(body, "copy_entry_from")?;
            let Some(label) = body.get("artifact").and_then(J::as_str) else {
                return Err(AdapterError::unsupported("`copy_entry_from` carries no `artifact`"));
            };
            let Some(source) = artifacts.get(label) else {
                return Err(AdapterError::unsupported(format!("`copy_entry_from` names no artifact `{label}`")));
            };
            let source = super::rawzip::read_ordered(source).map_err(AdapterError::Host)?;
            let Some((_, data)) = source.iter().find(|(n, _)| n == path) else {
                return Err(AdapterError::unsupported(format!(
                    "`copy_entry_from` finds no entry `{path}` in `{label}`"
                )));
            };
            set_entry(entries, path, data.clone());
            Ok(())
        }
        // Add an `entries` manifest member for `path` with the correct media and
        // sha256 of the current bytes.
        "add_manifest_entry" => {
            let path = entry_path(body, "add_manifest_entry")?;
            let sha = entries
                .iter()
                .find(|(n, _)| n == path)
                .map(|(_, data)| Digest::of_bytes(data).to_canonical_text());
            let Some(sha) = sha else {
                return Err(AdapterError::unsupported(format!("`add_manifest_entry` names absent entry `{path}`")));
            };
            edit_manifest(entries, |manifest| {
                if let Some(map) = manifest.get_mut("entries").and_then(J::as_object_mut) {
                    map.insert(
                        path.to_owned(),
                        serde_json::json!({ "media": "application/octet-stream", "sha256": sha }),
                    );
                }
            })
        }
        // Set (creating if absent) an entry's JSON member at a pointer, leaving the
        // manifest checksum stale unless a later `fix_checksums` recomputes it.
        "edit_json" => {
            let path = entry_path(body, "edit_json")?;
            let Some(pointer) = body.get("pointer").and_then(J::as_str) else {
                return Err(AdapterError::unsupported("`edit_json` carries no `pointer`"));
            };
            let new = body.get("value").cloned().unwrap_or(J::Null);
            let Some((_, data)) = entries.iter_mut().find(|(n, _)| n == path) else {
                return Err(AdapterError::unsupported(format!("`edit_json` names absent entry `{path}`")));
            };
            let mut value: J = serde_json::from_slice(data).map_err(|err| AdapterError::Host(err.to_string()))?;
            set_json_pointer(&mut value, pointer, new)?;
            *data = serde_json::to_vec(&value).map_err(|err| AdapterError::Host(err.to_string()))?;
            Ok(())
        }
        // Replace an identifier everywhere it appears as a string value or member
        // name in the artifact's JSON entries (manifest, history index, definition).
        "rewrite_identifier" => {
            let Some(from) = body.get("from").and_then(J::as_str) else {
                return Err(AdapterError::unsupported("`rewrite_identifier` carries no `from`"));
            };
            let Some(to) = body.get("to").and_then(J::as_str) else {
                return Err(AdapterError::unsupported("`rewrite_identifier` carries no `to`"));
            };
            for (_, data) in entries.iter_mut().filter(|(name, _)| name.ends_with(".json")) {
                if let Ok(mut value) = serde_json::from_slice::<J>(data) {
                    rewrite_identifier(&mut value, from, to);
                    if let Ok(bytes) = serde_json::to_vec(&value) {
                        *data = bytes;
                    }
                }
            }
            Ok(())
        }
        // Recompute every byte checksum so tampered bytes are self-consistent.
        "fix_checksums" => rehash(entries),
        "edit_cbor" => Err(AdapterError::unsupported(
            "`tamper_artifact` op `edit_cbor` needs schema-owned resolution of a keyed-collection \
             logical pointer (`/state/<coll>/<key>/…`) into the state section, beyond byte surgery",
        )),
        "duplicate_json_member" => Err(AdapterError::unsupported(
            "`tamper_artifact` op `duplicate_json_member` targets `history/index.json` ranges, which \
             the runtime emits as an empty object (a CORE simplification), so there is nothing to \
             duplicate and §19.6 range-partition verification is unlanded",
        )),
        other => Err(AdapterError::unsupported(format!(
            "`tamper_artifact` op `{other}` is not driven this phase"
        ))),
    }
}

/// The `path` member of a tamper op, or a precise skip when absent.
fn entry_path<'a>(body: &'a J, op: &str) -> Result<&'a str, AdapterError> {
    body.get("path")
        .and_then(J::as_str)
        .ok_or_else(|| AdapterError::unsupported(format!("`{op}` carries no `path`")))
}

/// Parse `manifest.json`, apply `edit`, and write it back.
fn edit_manifest(entries: &mut [(String, Vec<u8>)], edit: impl FnOnce(&mut J)) -> Result<(), AdapterError> {
    let Some((_, data)) = entries.iter_mut().find(|(n, _)| n == MANIFEST_JSON) else {
        return Err(AdapterError::unsupported("op finds no `manifest.json` entry"));
    };
    let mut manifest: J = serde_json::from_slice(data).map_err(|err| AdapterError::Host(err.to_string()))?;
    edit(&mut manifest);
    *data = serde_json::to_vec(&manifest).map_err(|err| AdapterError::Host(err.to_string()))?;
    Ok(())
}

/// Set a JSON pointer, creating any absent intermediate object members and the
/// final member. Supports the object and array-index pointer forms the corpus uses;
/// `~1`/`~0` unescape to `/`/`~` (RFC 6901).
fn set_json_pointer(root: &mut J, pointer: &str, new: J) -> Result<(), AdapterError> {
    let segments: Vec<String> = pointer
        .strip_prefix('/')
        .unwrap_or(pointer)
        .split('/')
        .map(|segment| segment.replace("~1", "/").replace("~0", "~"))
        .collect();
    let Some((last, parents)) = segments.split_last() else {
        *root = new;
        return Ok(());
    };
    if last.is_empty() && parents.is_empty() {
        *root = new;
        return Ok(());
    }
    let mut current = root;
    for segment in parents {
        current = descend(current, segment)?;
    }
    match current {
        J::Object(map) => {
            map.insert(last.clone(), new);
            Ok(())
        }
        J::Array(items) => match last.parse::<usize>().ok().and_then(|index| items.get_mut(index)) {
            Some(slot) => {
                *slot = new;
                Ok(())
            }
            None => Err(AdapterError::unsupported(format!("`edit_json` pointer index `{last}` is out of range"))),
        },
        _ => Err(AdapterError::unsupported(format!("`edit_json` pointer `{pointer}` is not addressable"))),
    }
}

/// Descend one pointer segment, creating an absent object member as an empty object.
fn descend<'a>(current: &'a mut J, segment: &str) -> Result<&'a mut J, AdapterError> {
    match current {
        J::Object(map) => Ok(map.entry(segment.to_owned()).or_insert_with(|| J::Object(serde_json::Map::new()))),
        J::Array(items) => match segment.parse::<usize>().ok().and_then(|index| items.get_mut(index)) {
            Some(slot) => Ok(slot),
            None => Err(AdapterError::unsupported(format!("`edit_json` pointer index `{segment}` is out of range"))),
        },
        _ => Err(AdapterError::unsupported("`edit_json` pointer descends through a non-container")),
    }
}

/// Replace every string value and object member name that *equals* `from` with `to`,
/// recursively — the identifier-substitution the §19 point-id aliasing tamper needs.
fn rewrite_identifier(value: &mut J, from: &str, to: &str) {
    match value {
        J::String(text) => {
            if text == from {
                *text = to.to_owned();
            }
        }
        J::Array(items) => {
            for item in items {
                rewrite_identifier(item, from, to);
            }
        }
        J::Object(map) => {
            let mut rebuilt = serde_json::Map::with_capacity(map.len());
            for (key, mut member) in std::mem::take(map) {
                rewrite_identifier(&mut member, from, to);
                let key = if key == from { to.to_owned() } else { key };
                rebuilt.insert(key, member);
            }
            *map = rebuilt;
        }
        _ => {}
    }
}

/// Match `name` against a glob with at most one `*` wildcard (prefix `*` suffix).
fn glob_match(glob: &str, name: &str) -> bool {
    match glob.split_once('*') {
        None => glob == name,
        Some((prefix, suffix)) => {
            name.len() >= prefix.len() + suffix.len()
                && name.starts_with(prefix)
                && name.ends_with(suffix)
        }
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
