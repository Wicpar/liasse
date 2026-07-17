//! Wiring the managed [`Keyring`] into the evaluation environment (§17.2): the
//! version-metadata rows a keyring public selector reads.
//!
//! §17.2 exposes a keyring as a view of version-metadata rows, and
//! `.$current`/`.$accepted`/`.$public`/`.$versions` name lifecycle subsets of
//! that view. The runtime [`Keyring`] already computes the lifecycle
//! (`current`/`accepted`/`versions`); this module renders each version into a
//! [`Row`] and snapshots the active/accepted classification at the read instant,
//! so [`RuntimeEnv::keyring`](crate::env::RuntimeEnv) can answer a selector by a
//! pure lookup. It also parses a `$keyring` declaration into a [`KeyringPolicy`]
//! and builds the in-process [`SimKeyProvider`] the engine bootstraps against,
//! so a package that declares a keyring gets a live ring without external host
//! wiring.

use std::collections::BTreeSet;

use liasse_expr::{Cell, Row, RowId};
use liasse_host::sim::SimKeyProvider;
use liasse_host::{KeyCapabilities, KeyOperation, KeyProvider, ProtectionClass};
use liasse_value::{Duration, Integer, Text, Value};

use crate::doc;
use crate::keyring::{
    KeyVersion, Keyring, KeyringPolicy, RotationMode, RotationSchedule, VersionId,
};

/// A rendered, read-time snapshot of one keyring's version view (§17.2): every
/// version as a metadata [`Row`], plus the identities that are the active
/// (`.$current`) and accepted (`.$accepted`/`.$public`) subsets at the read
/// instant. `.$versions` is every row.
#[derive(Debug, Clone)]
pub(crate) struct KeyringSnapshot {
    /// The ring name (the root member the version collection materializes under).
    pub(crate) name: String,
    /// Every retained version as a metadata row, in version order.
    pub(crate) rows: Vec<Row>,
    /// The active version's row identity (`.$current`); at most one (§17.3).
    active: BTreeSet<RowId>,
    /// The version identities accepted for verification at the read instant
    /// (`.$accepted`/`.$public`).
    accepted: BTreeSet<RowId>,
}

impl KeyringSnapshot {
    /// Snapshot `ring`'s version view at `now` (§17.2): render each version and
    /// classify the active and accepted subsets against the read instant.
    pub(crate) fn of<P: KeyProvider>(ring: &Keyring<P>, now: liasse_value::Timestamp) -> Self {
        let name = ring.name().to_owned();
        let rows: Vec<Row> = ring.versions().iter().map(|v| version_row(&name, v)).collect();
        let active = ring
            .current()
            .map(|v| row_id(&name, v.id()))
            .into_iter()
            .collect();
        let accepted = ring.accepted(now).iter().map(|v| row_id(&name, v.id())).collect();
        Self { name, rows, active, accepted }
    }

    /// Whether `base` is exactly this ring's version view: a keyring selector's
    /// base evaluates to the ring's full version collection, so its row
    /// identities are exactly this snapshot's rows.
    fn matches(&self, base: &[Row]) -> bool {
        let base_ids: BTreeSet<&RowId> = base.iter().map(Row::id).collect();
        let own_ids: BTreeSet<&RowId> = self.rows.iter().map(Row::id).collect();
        base_ids == own_ids
    }

    /// The rows this snapshot exposes for `active`/`accepted` selection.
    fn subset(&self, ids: &BTreeSet<RowId>) -> Vec<Row> {
        self.rows.iter().filter(|row| ids.contains(row.id())).cloned().collect()
    }

    /// The active version rows (`.$current`).
    pub(crate) fn current(&self) -> Vec<Row> {
        self.subset(&self.active)
    }

    /// The accepted version rows (`.$accepted`/`.$public`).
    pub(crate) fn accepted_rows(&self) -> Vec<Row> {
        self.subset(&self.accepted)
    }
}

/// The snapshot owning `base` (the ring whose version view `base` is), if any.
pub(crate) fn snapshot_for<'a>(
    snapshots: &'a [KeyringSnapshot],
    base: &[Row],
) -> Option<&'a KeyringSnapshot> {
    snapshots.iter().find(|snap| snap.matches(base))
}

/// The stable identity of one version row (Annex D.1): the ring name and the
/// version ordinal, so a snapshot row and its materialized collection twin share
/// one identity and a selector matches by it.
fn row_id(name: &str, id: VersionId) -> RowId {
    RowId::keyed(format!("{name}#v{}", id.get()))
}

/// One version's §17.2 public metadata as a row. An absent optional member
/// (`activated_at` while pending, `retired_at`/`revoked_at` before those
/// transitions, `attestation`) is a `none` cell, which the view materializer
/// renders as an omitted member (§A.9) — so a pending version has no
/// `activated_at`, matching the corpus `$absent` expectations. Private key bytes
/// never appear; only the public key material does (§17.2).
fn version_row(name: &str, version: &KeyVersion) -> Row {
    let id = row_id(name, version.id());
    let ordinal = Value::Int(Integer::from(i64::try_from(version.id().get()).unwrap_or(i64::MAX)));
    let cells = [
        ("id".to_owned(), Cell::Scalar(ordinal.clone())),
        ("algorithm".to_owned(), Cell::Scalar(Value::Text(Text::new(version.algorithm().to_owned())))),
        ("public_key".to_owned(), Cell::Scalar(version.public_key().clone())),
        ("created_at".to_owned(), Cell::Scalar(Value::Timestamp(version.created_at()))),
        ("activated_at".to_owned(), optional_ts(version.activated_at())),
        ("retired_at".to_owned(), optional_ts(version.retired_at())),
        ("revoked_at".to_owned(), optional_ts(version.revoked_at())),
        ("attestation".to_owned(), Cell::Scalar(version.attestation().cloned().unwrap_or(Value::None))),
    ];
    Row::new(id, ordinal, cells)
}

/// A present timestamp cell, or a `none` cell (rendered as an omitted member).
fn optional_ts(instant: Option<liasse_value::Timestamp>) -> Cell {
    Cell::Scalar(instant.map_or(Value::None, Value::Timestamp))
}

/// The deterministic external key handle the self-provisioned provider carries
/// for a manual-rotation keyring (§17.4 manual policy). A manual keyring is
/// activated only by an operator binding an externally created handle, so the
/// engine's own provider must offer one for
/// [`Engine::keyring_admin`](crate::Engine::keyring_admin)`.bind_activate` to have
/// anything to bind. A driver references it as
/// `ExternalKeyRef::new(MANUAL_EXTERNAL_KEY)`.
pub const MANUAL_EXTERNAL_KEY: &str = "liasse.manual-bootstrap";

/// The in-process key provider the engine bootstraps a declared keyring against
/// when no external host provider is registered. It advertises everything the
/// declared policy needs — the declared algorithm, every protected operation,
/// generation, binding, disable, destroy, and attestation, at hardware
/// protection — so the §17.6 capability check passes and the deterministic
/// double drives the version lifecycle. A manual policy additionally carries one
/// bindable [`MANUAL_EXTERNAL_KEY`] handle in the declared algorithm, so an
/// operator can bind and activate the first version through
/// [`Engine::keyring_admin`](crate::Engine::keyring_admin) (§17.4 manual policy).
pub(crate) fn built_in_provider(policy: &KeyringPolicy) -> SimKeyProvider {
    let mut caps = KeyCapabilities::builder(ProtectionClass::Hardware)
        .algorithm(policy.algorithm.clone())
        .generates()
        .binds()
        .disables()
        .destroys()
        .attests();
    for op in [
        KeyOperation::Sign,
        KeyOperation::Verify,
        KeyOperation::Decrypt,
        KeyOperation::KeyAgreement,
        KeyOperation::Wrap,
        KeyOperation::Mac,
    ] {
        caps = caps.operation(op);
    }
    let provider = SimKeyProvider::new(caps.build());
    if matches!(policy.rotate.map(|r| r.mode), Some(RotationMode::Manual)) {
        provider.with_external_key(MANUAL_EXTERNAL_KEY, policy.algorithm.clone())
    } else {
        provider
    }
}

/// Parse a `$keyring` policy object (§17.1, C.16) into a [`KeyringPolicy`], or
/// `None` when the declaration is malformed enough that the model would have
/// rejected it (the model validates the shape, so a well-loaded package reaches
/// this with a parseable declaration).
pub(crate) fn policy_from_doc(keyring: &liasse_syntax::DocValue) -> Option<KeyringPolicy> {
    let algorithm = doc::member(keyring, "$algorithm").and_then(doc::string)?.to_owned();
    let usage = doc::member(keyring, "$usage")
        .and_then(doc::array)
        .map(|items| items.iter().filter_map(doc::string).filter_map(operation).collect())
        .unwrap_or_default();
    let rotate = doc::member(keyring, "$rotate").and_then(rotation_schedule);
    let retain = doc::member(keyring, "$retain").and_then(doc::string).and_then(|t| Duration::parse(t).ok());
    let protection = doc::member(keyring, "$protection").and_then(doc::string).and_then(protection_class);
    Some(KeyringPolicy { algorithm, usage, rotate, retain, protection })
}

/// A `$rotate` schedule: a bare duration string is automatic rotation at that
/// cadence with zero overlap; an object form reads `$every`/`$overlap`/`$mode`.
fn rotation_schedule(rotate: &liasse_syntax::DocValue) -> Option<RotationSchedule> {
    if let Some(text) = doc::string(rotate) {
        let every = Duration::parse(text).ok()?;
        return Some(RotationSchedule { every, overlap: Duration::ZERO, mode: RotationMode::Automatic });
    }
    let every = Duration::parse(doc::member(rotate, "$every").and_then(doc::string)?).ok()?;
    let overlap = doc::member(rotate, "$overlap")
        .and_then(doc::string)
        .and_then(|t| Duration::parse(t).ok())
        .unwrap_or(Duration::ZERO);
    let mode = match doc::member(rotate, "$mode").and_then(doc::string) {
        Some("manual") => RotationMode::Manual,
        _ => RotationMode::Automatic,
    };
    Some(RotationSchedule { every, overlap, mode })
}

/// A `$usage` operation name (§17.1).
fn operation(name: &str) -> Option<KeyOperation> {
    Some(match name {
        "sign" => KeyOperation::Sign,
        "verify" => KeyOperation::Verify,
        "decrypt" => KeyOperation::Decrypt,
        "key_agreement" | "keyAgreement" => KeyOperation::KeyAgreement,
        "wrap" => KeyOperation::Wrap,
        "mac" => KeyOperation::Mac,
        _ => return None,
    })
}

/// A `$protection` class (§17.1).
fn protection_class(name: &str) -> Option<ProtectionClass> {
    Some(match name {
        "software" => ProtectionClass::Software,
        "hardware" => ProtectionClass::Hardware,
        _ => return None,
    })
}
