//! Parsing `manifest.json` into the typed [`Manifest`](super::Manifest),
//! enforcing the closed format-1 structure (SPEC.md §19.5).
//!
//! The [`Obj`] cursor tracks each object's path so a rejection names the exact
//! offending member (`entries/state/current.cbor.zst/sha256`, not just
//! "sha256"). Parsing runs over a `serde_json::Value`; every member outside the
//! per-object allow-list is rejected ("Additional members are invalid for
//! format version 1").

use std::collections::BTreeMap;

use liasse_ident::{DefinitionId, Digest, HistoryPoint, InstanceId, LineageId, PointId};
use serde_json::{Map, Value};

use crate::error::ArtifactError;

use super::{
    DefinitionRef, EntryChecksum, EntryRef, IncludedModule, Manifest, MountRef, TOP_MEMBERS,
};

impl Manifest {
    /// Parse `manifest.json` bytes into the typed model, enforcing the closed
    /// format-1 structure.
    pub fn parse(bytes: &[u8]) -> Result<Self, ArtifactError> {
        let value: Value = serde_json::from_slice(bytes).map_err(|e| ArtifactError::ManifestJson {
            detail: e.to_string(),
        })?;
        let root = Obj::root(&value)?;
        root.reject_unknown(TOP_MEMBERS)?;
        root.require_format()?;

        let instance = InstanceId::new(root.str_member("instance")?);
        let selected = read_point(&root.object_member("selected")?)?;
        let definition = read_definition(&root.object_member("definition")?)?;
        let state = read_entry_ref(&root.object_member("state")?)?;
        let history = read_entry_ref(&root.object_member("history")?)?;
        let modules = read_modules(root.optional_object("modules")?)?;
        let included_modules = read_included(root.optional_object("included_modules")?)?;
        let entries = read_entries(&root.object_member("entries")?)?;

        Ok(Self {
            instance,
            selected,
            definition,
            state,
            history,
            modules,
            included_modules,
            entries,
        })
    }
}

/// A parse cursor over one JSON object, tracking its path for diagnostics.
struct Obj<'a> {
    map: &'a Map<String, Value>,
    path: String,
}

impl<'a> Obj<'a> {
    fn root(value: &'a Value) -> Result<Self, ArtifactError> {
        Self::of(value, String::new(), "manifest.json")
    }

    fn of(value: &'a Value, path: String, label: &str) -> Result<Self, ArtifactError> {
        match value.as_object() {
            Some(map) => Ok(Self { map, path }),
            None => Err(ArtifactError::ManifestBadValue {
                member: if path.is_empty() { label.to_owned() } else { path },
                detail: "expected a JSON object".to_owned(),
            }),
        }
    }

    fn child_path(&self, name: &str) -> String {
        if self.path.is_empty() {
            name.to_owned()
        } else {
            format!("{}/{name}", self.path)
        }
    }

    fn require_format(&self) -> Result<(), ArtifactError> {
        let value = self.map.get("format").ok_or(ArtifactError::ManifestMissingMember {
            member: "format",
        })?;
        let found = value.as_i64().ok_or_else(|| ArtifactError::ManifestBadValue {
            member: self.child_path("format"),
            detail: "expected an integer".to_owned(),
        })?;
        if found == 1 {
            Ok(())
        } else {
            Err(ArtifactError::ManifestFormatUnsupported { found })
        }
    }

    fn reject_unknown(&self, allowed: &[&str]) -> Result<(), ArtifactError> {
        for name in self.map.keys() {
            if !allowed.contains(&name.as_str()) {
                return Err(ArtifactError::ManifestUnknownMember {
                    name: self.child_path(name),
                });
            }
        }
        Ok(())
    }

    fn require(&self, name: &'static str) -> Result<&'a Value, ArtifactError> {
        self.map
            .get(name)
            .ok_or(ArtifactError::ManifestMissingMember { member: name })
    }

    fn str_member(&self, name: &'static str) -> Result<&'a str, ArtifactError> {
        let value = self.require(name)?;
        value.as_str().ok_or_else(|| ArtifactError::ManifestBadValue {
            member: self.child_path(name),
            detail: "expected a string".to_owned(),
        })
    }

    fn object_member(&self, name: &'static str) -> Result<Obj<'a>, ArtifactError> {
        let value = self.require(name)?;
        Obj::of(value, self.child_path(name), name)
    }

    fn optional_object(&self, name: &'static str) -> Result<Option<Obj<'a>>, ArtifactError> {
        match self.map.get(name) {
            None => Ok(None),
            Some(value) => Obj::of(value, self.child_path(name), name).map(Some),
        }
    }

    fn digest_member(&self, name: &'static str) -> Result<Digest, ArtifactError> {
        Digest::parse(self.str_member(name)?).map_err(ArtifactError::Digest)
    }

    /// Read a nested object addressed by a runtime-supplied key (map member).
    fn object_member_dynamic(&self, name: &str) -> Result<Obj<'a>, ArtifactError> {
        let value = self
            .map
            .get(name)
            .ok_or(ArtifactError::ManifestMissingMember { member: "entry" })?;
        Obj::of(value, self.child_path(name), name)
    }
}

fn read_point(obj: &Obj<'_>) -> Result<HistoryPoint, ArtifactError> {
    obj.reject_unknown(&["lineage", "point"])?;
    let lineage = LineageId::new(obj.str_member("lineage")?);
    let point = PointId::new(obj.str_member("point")?);
    Ok(HistoryPoint::new(lineage, point))
}

fn read_definition(obj: &Obj<'_>) -> Result<DefinitionRef, ArtifactError> {
    obj.reject_unknown(&["identity", "path"])?;
    let identity =
        DefinitionId::parse(obj.str_member("identity")?).map_err(ArtifactError::Digest)?;
    Ok(DefinitionRef {
        identity,
        path: obj.str_member("path")?.to_owned(),
    })
}

fn read_entry_ref(obj: &Obj<'_>) -> Result<EntryRef, ArtifactError> {
    obj.reject_unknown(&["path", "sha256"])?;
    Ok(EntryRef {
        path: obj.str_member("path")?.to_owned(),
        sha256: obj.digest_member("sha256")?,
    })
}

fn read_modules(obj: Option<Obj<'_>>) -> Result<BTreeMap<String, MountRef>, ArtifactError> {
    let Some(obj) = obj else {
        return Ok(BTreeMap::new());
    };
    let mut out = BTreeMap::new();
    for name in obj.map.keys() {
        let mount = obj.object_member_dynamic(name)?;
        mount.reject_unknown(&["instance", "artifact", "selected"])?;
        let selected = read_point(&mount.object_member("selected")?)?;
        out.insert(
            name.clone(),
            MountRef {
                instance: InstanceId::new(mount.str_member("instance")?),
                artifact: mount.str_member("artifact")?.to_owned(),
                selected,
            },
        );
    }
    Ok(out)
}

fn read_included(
    obj: Option<Obj<'_>>,
) -> Result<BTreeMap<InstanceId, IncludedModule>, ArtifactError> {
    let Some(obj) = obj else {
        return Ok(BTreeMap::new());
    };
    let mut out = BTreeMap::new();
    for inc in obj.map.keys() {
        let entry = obj.object_member_dynamic(inc)?;
        entry.reject_unknown(&["artifact", "sha256"])?;
        out.insert(
            InstanceId::new(inc.as_str()),
            IncludedModule {
                artifact: entry.str_member("artifact")?.to_owned(),
                sha256: entry.digest_member("sha256")?,
            },
        );
    }
    Ok(out)
}

fn read_entries(obj: &Obj<'_>) -> Result<BTreeMap<String, EntryChecksum>, ArtifactError> {
    let mut out = BTreeMap::new();
    for path in obj.map.keys() {
        let entry = obj.object_member_dynamic(path)?;
        entry.reject_unknown(&["media", "sha256"])?;
        out.insert(
            path.clone(),
            EntryChecksum {
                media: entry.str_member("media")?.to_owned(),
                sha256: entry.digest_member("sha256")?,
            },
        );
    }
    Ok(out)
}
