//! §13.16 module VALUES at the lifecycle boundary.
//!
//! The value surface (`pack`, `update_module`, `rollback_module`) is a *spelling*
//! of the §13.10 lifecycle runtime, not a second mechanism. This module owns the
//! three things that spelling adds and nothing else:
//!
//! 1. reading a [`Value::Module`] operand into the instance (or the not-yet-
//!    materialized blob) it denotes;
//! 2. the **retention policy** — which of `pack`'s three axes and which rollback
//!    coordinates a CORE instance can actually honour, and a loud, named refusal
//!    for each one it cannot;
//! 3. the **ancestry divergence** an `update_module(… { migrate: model+data })`
//!    reports instead of merging.
//!
//! # The retention policy, stated once
//!
//! A CORE instance retains its **active definition** (`InstanceStore::definition`)
//! and its **selected point** (`Engine::export`'s coverage is the single-point
//! range `[selected, selected]`, and its `history/index.json` carries empty segment
//! ranges — a documented §19.4/§19.6 seam). The commit log records no per-point
//! definition, so the model in force at an earlier instant is not reconstructable
//! at all.
//!
//! §13.16 requires a historical extract to be **time-anchored**: "the state at a
//! time `@t` pairs with the model version in effect at `@t`". Because that pairing
//! is exactly what is not retained, addressing an interior instant is not a missing
//! implementation — it is unanswerable from what the instance holds. Every such
//! request is therefore refused by name, and is NEVER approximated to the nearest
//! retained point: a wrong module state that ships looking correct is the one
//! outcome this subsystem must not produce.

use liasse_ident::HistoryPoint;
use liasse_store::InstanceStore;
use liasse_value::{BlobDescriptor, MediaType, ModuleHandle, Period, Sha512, Timestamp, Value};

use crate::error::{EngineError, Rejection, RejectionReason};
use crate::history::ImportRelation;

/// The media type a packed `.liasse` artifact carries, the same one a package blob
/// is stored under.
pub(crate) const LIASSE_MEDIA_TYPE: &str = "application/vnd.liasse+zip";

/// What a [`Value::Module`] operand denotes at the lifecycle boundary (§13.16).
///
/// The two arms are the two halves of the blob boundary: a handle produced by
/// `unpack` carries undecoded bytes and has no instance yet; a handle to a
/// mounted instance addresses one by the rendered address of the
/// module-collection entry it occupies.
#[derive(Debug, Clone)]
pub(crate) enum ModuleOperand {
    /// A not-yet-materialized module (`unpack(blob)`): the source `.liasse` blob.
    /// Materialization stays DEFERRED — nothing here decodes it.
    Pending(Box<BlobDescriptor>),
    /// A live instance, addressed by the rendered address of its entry.
    Mounted(String),
}

impl ModuleOperand {
    /// Read a lifecycle argument as a module operand. A non-module value is an
    /// operand the CHECKER already refused (§13.16 types every operator's operands),
    /// so reaching this is an interpreter/host contract breach, reported as such
    /// rather than defaulted.
    pub(crate) fn read(args: &[(String, Value)], key: &str, operator: &str) -> Result<Self, Rejection> {
        match args.iter().find(|(name, _)| name == key).map(|(_, value)| value) {
            Some(Value::Module(ModuleHandle::Pending(descriptor))) => Ok(Self::Pending(descriptor.clone())),
            Some(Value::Module(ModuleHandle::Mounted(at))) => Ok(Self::Mounted(at.clone())),
            _ => Err(Rejection::new(
                RejectionReason::Malformed,
                format!("`{operator}` requires a `module` value as its `{key}` operand (§13.16)"),
            )),
        }
    }

    /// The mount this operand addresses, or a loud refusal when it is a pending
    /// handle. `update_module` and `rollback_module` act on a LIVE instance: a
    /// pending handle has no identity, no history and no mount, so there is nothing
    /// to update or roll back — refused rather than silently installing one.
    pub(crate) fn mount(&self, operator: &str) -> Result<&str, Rejection> {
        match self {
            Self::Mounted(at) => Ok(at.as_str()),
            Self::Pending(_) => Err(Rejection::new(
                RejectionReason::Malformed,
                format!(
                    "`{operator}` acts on an installed instance, but its operand is a module that \
                     `unpack` has not yet materialized: it has no instance identity, no mount and \
                     no retained history (§13.16). Install it first (`<-`), then {operator} it."
                ),
            )),
        }
    }
}

/// The three coordinates a `pack` addresses (§13.16): the definition by **version**,
/// the state by a **point in time**, the history by a **time range**. Each is
/// optional and defaults to the current value.
#[derive(Debug, Clone, Default)]
pub(crate) struct PackAxes {
    model: Option<String>,
    data: Option<Timestamp>,
    history: Option<Period>,
}

impl PackAxes {
    /// Read the three axes from a lifecycle call's evaluated arguments. An absent
    /// axis (`none`) is "the current value".
    pub(crate) fn read(args: &[(String, Value)]) -> Self {
        let find = |key: &str| args.iter().find(|(name, _)| name == key).map(|(_, value)| value);
        Self {
            model: match find(liasse_model::lifecycle_arg::MODEL) {
                Some(Value::Text(text)) => Some(text.as_str().to_owned()),
                _ => None,
            },
            data: match find(liasse_model::lifecycle_arg::DATA) {
                Some(Value::Timestamp(at)) => Some(*at),
                _ => None,
            },
            history: match find(liasse_model::lifecycle_arg::HISTORY) {
                Some(Value::Period(period)) => Some((**period).clone()),
                _ => None,
            },
        }
    }

    /// Whether every axis is at its default, i.e. the extract is "the current
    /// version, the current state, the full retained history" — the one combination
    /// a CORE instance retains, and the only one `Engine::export` produces.
    pub(crate) fn is_current(&self) -> bool {
        self.model.is_none() && self.data.is_none() && self.history.is_none()
    }

    /// Refuse every axis the instance cannot honour, naming the axis, the coordinate
    /// asked for, what IS retained, and why the two cannot be reconciled.
    ///
    /// `active` is the instance's active package version text and `selected` its
    /// selected history point, so the refusal states the alternative the caller
    /// actually has rather than a bare "unsupported".
    pub(crate) fn admit(&self, active: [u64; 3], selected: &HistoryPoint) -> Result<(), EngineError> {
        let active = format!("{}.{}.{}", active[0], active[1], active[2]);
        if let Some(model) = &self.model
            && *model != active
        {
            return Err(EngineError::Unsupported(format!(
                "`pack`'s `model` axis asks for package version `{model}`, but the instance \
                 retains only its ACTIVE definition (`{active}`) — the commit log records no \
                 per-point definition (§19.4/§19.6), so an earlier version's model cannot be \
                 reconstructed. Refused rather than emitting the active definition under a \
                 version label it does not carry (§13.16)."
            )));
        }
        if let Some(at) = &self.data {
            return Err(EngineError::Unsupported(format!(
                "`pack`'s `data` axis asks for the state at `{}`, but a CORE export carries \
                 exactly the SELECTED point `{}` (its coverage states the single-point range \
                 `[selected, selected]` and its history index carries empty segment ranges, \
                 §19.4/§19.6). §13.16 requires a historical extract to be time-anchored — the \
                 state at `@t` paired with the model version in effect at `@t` — and that pairing \
                 is not retained. Refused, and deliberately NOT snapped to the nearest retained \
                 point.",
                at.to_canonical_text(),
                selected.point().as_str(),
            )));
        }
        if self.history.is_some() {
            return Err(EngineError::Unsupported(
                "`pack`'s `history` axis asks for a span of retained history, but a CORE export \
                 carries no reversible-compaction segments — its history index states empty \
                 ranges (§19.4/§19.6), so the only span it can honestly claim is the selected \
                 point itself. Refused rather than emitting an artifact whose coverage claims a \
                 span it does not hold (§13.16)."
                    .to_owned(),
            ));
        }
        Ok(())
    }
}

/// The retained-point coordinate a `rollback_module` addresses (§13.16).
///
/// A rollback needs the definition AND the state at the target point. Only two
/// things carry both: the instance's own selected point (a no-op rollback), and a
/// `.liasse` artifact previously produced AT that point — which is exactly what
/// `pack` returns. A bare instant addresses neither, because no per-point
/// definition is retained.
#[derive(Debug, Clone)]
pub(crate) enum RollbackPoint {
    /// The artifact carrying the retained point, verified and classified by
    /// [`Engine::classify`](crate::Engine::classify) before anything moves.
    Artifact(Box<BlobDescriptor>),
    /// A bare instant — refused; see [`Self::unsupported_instant`].
    Instant(Timestamp),
}

impl RollbackPoint {
    /// Read the coordinate operand. The checker admitted exactly a `timestamp` or a
    /// `blob`, so any other value is a contract breach.
    pub(crate) fn read(args: &[(String, Value)]) -> Result<Self, Rejection> {
        match args.iter().find(|(name, _)| name == liasse_model::lifecycle_arg::POINT).map(|(_, value)| value) {
            Some(Value::Blob(descriptor)) => Ok(Self::Artifact(descriptor.clone())),
            Some(Value::Timestamp(at)) => Ok(Self::Instant(*at)),
            _ => Err(Rejection::new(
                RejectionReason::Malformed,
                "`rollback_module` addresses a retained point by `timestamp` or by the `blob` \
                 artifact carrying it (§13.16)"
                    .to_owned(),
            )),
        }
    }

    /// The loud refusal for a bare instant: it names the instant, the selected
    /// point, and the coordinate that WOULD work.
    pub(crate) fn unsupported_instant(at: Timestamp, selected: &HistoryPoint) -> EngineError {
        EngineError::Unsupported(format!(
            "`rollback_module` asks to fork back to the instant `{}`, but the instance retains \
             only its selected point `{}`: an earlier point's DEFINITION is not retained (the \
             commit log records no per-point definition, §19.4/§19.6), so its model and state \
             cannot be paired as §13.16 requires. Refused, and deliberately NOT snapped to the \
             nearest retained point — address the point by the `.liasse` artifact `pack` produced \
             at it, which carries both halves.",
            at.to_canonical_text(),
            selected.point().as_str(),
        ))
    }
}

/// A refused `update_module(… { migrate: model+data })` (§13.16): the two histories
/// do not stand in an ancestor relation, so the update is not a fast-forward.
///
/// §13.16 is explicit that "reconciling a divergent history is performed outside the
/// engine — the engine offers no in-language merge". This type is what the engine
/// hands the external tool instead: the classification it reached, which instance it
/// concerns, and the two points, so the resolution can be computed without
/// re-deriving any of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AncestryDivergence {
    /// How the incoming history related to the live instance's (§19.8).
    pub relation: ImportRelation,
    /// The address of the module-collection entry the live instance is mounted at.
    pub at: String,
    /// The live instance's selected point.
    pub local: HistoryPoint,
    /// The incoming artifact's selected point.
    pub incoming: HistoryPoint,
}

impl AncestryDivergence {
    /// Whether a classification is a fast-forward the update applies automatically.
    /// `SamePoint` is included: the incoming state is already the live one, so the
    /// update is a genuine no-op rather than a movement.
    pub(crate) fn is_fast_forward(relation: ImportRelation) -> bool {
        matches!(relation, ImportRelation::FastForward | ImportRelation::SamePoint)
    }

    /// Why the two histories could not be reconciled automatically, in the terms
    /// §19.8 classifies them by.
    fn cause(&self) -> &'static str {
        match self.relation {
            ImportRelation::Merge => {
                "the two histories share an ancestor and then DIVERGED, so neither state \
                 supersedes the other"
            }
            ImportRelation::Rollback => {
                "the incoming history PRECEDES the live one, so applying it would silently \
                 discard committed state"
            }
            ImportRelation::Unrelated => {
                "the incoming history shares NO point with the live one (a different instance, \
                 or a lineage this instance's ancestry does not know)"
            }
            ImportRelation::FastForward | ImportRelation::SamePoint => {
                "the histories are reconcilable and this is not a divergence"
            }
        }
    }
}

impl std::fmt::Display for AncestryDivergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`update_module(… {{ migrate: model+data }})` on `{}` is refused: {} \
             (relation {:?}; live point `{}/{}`, incoming point `{}/{}`). §13.16 reconciles a \
             divergent history OUTSIDE the engine — there is no in-language merge, and the \
             update is never applied silently.",
            self.at,
            self.cause(),
            self.relation,
            self.local.lineage().as_str(),
            self.local.point().as_str(),
            self.incoming.lineage().as_str(),
            self.incoming.point().as_str(),
        )
    }
}

impl From<AncestryDivergence> for Rejection {
    fn from(divergence: AncestryDivergence) -> Self {
        Self::new(RejectionReason::Compatibility, divergence.to_string())
    }
}

/// Fetch a blob's bytes from `store` (§18.3), refusing loudly when the store does
/// not hold them — never treating an unavailable package as an empty one.
pub(crate) fn blob_bytes<S: InstanceStore>(store: &S, descriptor: &BlobDescriptor) -> Result<Vec<u8>, Rejection> {
    store
        .get_blob(descriptor.sha512())
        .map_err(|error| Rejection::new(RejectionReason::Evaluation, format!("blob store error: {error}")))?
        .ok_or_else(|| {
            Rejection::new(
                RejectionReason::Malformed,
                "the `.liasse` blob is not held by the store, so its bytes cannot be read (§18.3)".to_owned(),
            )
        })
}

/// The content-addressed descriptor of freshly produced `.liasse` bytes. The digest
/// is computed here so `pack` can hand the caller a real descriptor during staging,
/// before the transition has committed and the bytes have landed in blob storage.
pub(crate) fn liasse_descriptor(bytes: &[u8], name: Option<String>) -> BlobDescriptor {
    BlobDescriptor::new(Sha512::of(bytes), bytes.len() as u64, MediaType::new(LIASSE_MEDIA_TYPE), name)
}
