//! What a lifecycle call records while a transition is being staged (§13.10,
//! §13.16).
//!
//! The host-privileged handle lent into a root/host-scope program runs HERE, during
//! staging, where it may only READ: it decodes a package, exports an instance, and
//! classifies an incoming history — then records a [`LifecycleIntent`] for the
//! module host to perform in the commit phase, so every lifecycle change folds into
//! the same atomic transition as the parent's own change. Nothing durable is
//! touched while staging, so a rejected transition leaves every instance exactly as
//! it was.
//!
//! Both spellings of the lifecycle land here — the declarative `module.install`/
//! `module.update`/`module.remove` of §13.10 and the value-surface `pack`/
//! `update_module`/`rollback_module` of §13.16 — because they are one runtime seen
//! from two vantage points.

use std::cell::RefCell;

use liasse_artifact::{decode_package_from_blob, Artifact};
use liasse_expr::Cell;
use liasse_ident::HistoryPoint;
use liasse_model::{lifecycle_arg as arg, LifecycleOp, MigrateAxis};
use liasse_store::{InstanceStore, RowAddress};
use liasse_value::{Text, Value};

use crate::dispatch::Lifecycle;
use crate::engine::Engine;
use crate::error::{EngineError, Rejection, RejectionReason};
use crate::history::ImportRelation;
use crate::modules::address;
use crate::modules::host::DecodedPackageId;
use crate::modules::value::{
    blob_bytes, liasse_descriptor, AncestryDivergence, ModuleOperand, PackAxes, RollbackPoint,
};

/// One live instance a §13.16 operator may address while staging: its mount and its
/// engine, borrowed read-only. The host builds this view over its mounted children
/// so the recorder reads instances without owning the host's own row type.
pub(super) struct MountedInstance<'a, S: InstanceStore> {
    /// The module-collection entry the instance is mounted at (§13.2/§13.3).
    pub(super) at: &'a RowAddress,
    /// The instance's own engine.
    pub(super) engine: &'a Engine<S>,
    /// Whether the instance's boundary is active (§13.3/§13.12).
    pub(super) enabled: bool,
}

/// A lifecycle intent recorded during staging (§13.10, §13.16): what to
/// mount, migrate, remove, move, or land once the shared commit succeeds.
pub(super) enum LifecycleIntent {
    /// Install a new instance from the decoded package definition. `occupant` is
    /// §13.16's "Install / override" half: a `<-` move into an occupied slot drops
    /// the instance already there, where a declarative `module.install` refuses the
    /// duplicate name (§13.3).
    Install {
        at: RowAddress,
        definition: String,
        package: DecodedPackageId,
        occupant: Occupant,
    },
    /// Relocate an installed instance to another entry of the SAME module
    /// collection (§13.16 "Move"): a §13.3 rekey that preserves the incarnation,
    /// and therefore the durable identity, while emptying the source entry.
    Relocate { from: RowAddress, to: RowAddress, occupant: Occupant },
    /// Re-admit an installed instance at an entry of a DIFFERENT module collection
    /// (§13.16 `reinstall_module`): the destination declares its own §13.4 parent
    /// surfaces, §13.5 peer set and §13.8 interface contracts, so the instance is
    /// put through admission against them exactly as an install is. It is never a
    /// rekey underneath a boundary that was not checked.
    Reinstall { from: RowAddress, to: RowAddress, occupant: Occupant },
    /// Update an existing instance to the decoded package definition (§20.1 chain).
    Update { at: RowAddress, definition: String, package: DecodedPackageId },
    /// Remove an existing instance (§13.12).
    Remove { at: RowAddress },
    /// Land freshly packed `.liasse` bytes in the root's §18.3 blob storage. The
    /// descriptor was handed to the caller during staging (its digest is a pure
    /// function of the bytes), so this only makes those bytes fetchable — and only
    /// once the transition has committed, so a rejected transition stores nothing.
    Pack { bytes: Vec<u8> },
    /// Move an instance to the history point an artifact carries (§19.8): the
    /// fast-forward half of `update_module(… { migrate: model+data })`, or the fork
    /// of `rollback_module`. The classification was established during staging, so a
    /// divergence had already rejected the transition before anything committed.
    Movement { at: RowAddress, artifact: Vec<u8>, relation: ImportRelation },
}

/// What a write into an already-occupied module slot does to the instance already
/// there (§13.3 vs §13.16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Occupant {
    /// §13.3: an instance name is "unique within its module collection", so the
    /// declarative `module.install` refuses a name already in use.
    Refuse,
    /// §13.16: "Moving a module value into a slot installs it, replacing any
    /// instance already there … an occupant is dropped and uninstalled."
    Drop,
}

/// The host-privileged lifecycle handle lent into the root/host-scope program
/// (§13.10, §13.16). It reads — decoding a blob, exporting an instance, classifying
/// an incoming history — and records a [`LifecycleIntent`]; the module host performs
/// the effect in the commit phase. It borrows the root store read-only to fetch blob
/// bytes (§18.3) and the mounted instances read-only to reach their engines, so
/// staging touches no durable state.
pub(super) struct LifecycleRecorder<'a, S: InstanceStore> {
    pub(super) blobs: &'a S,
    pub(super) instances: Vec<MountedInstance<'a, S>>,
    pub(super) intents: RefCell<Vec<LifecycleIntent>>,
}

impl<'a, S: InstanceStore> LifecycleRecorder<'a, S> {
    /// A recorder over the root store's blobs and the mounted instances.
    pub(super) fn new(blobs: &'a S, instances: Vec<MountedInstance<'a, S>>) -> Self {
        Self { blobs, instances, intents: RefCell::new(Vec::new()) }
    }

    /// The recorded intents, consuming the recorder.
    pub(super) fn into_intents(self) -> Vec<LifecycleIntent> {
        self.intents.into_inner()
    }

    /// The argument members each operation supports. `install`/`update` name the
    /// mount and carry the package `blob` (§13.10); the §13.16 value-surface
    /// operators address a module VALUE and carry their own axes instead.
    fn supported_args(op: LifecycleOp) -> &'static [&'static str] {
        match op {
            LifecycleOp::Install => &[arg::AT, "blob"],
            LifecycleOp::Update => &[arg::MODULE, "blob"],
            LifecycleOp::Remove => &[arg::MODULE],
            LifecycleOp::InstallModule => &[arg::MODULE],
            LifecycleOp::Reinstall => &[arg::MODULE],
            LifecycleOp::Pack => &[arg::MODULE, arg::MODEL, arg::DATA, arg::HISTORY],
            LifecycleOp::UpdateModule => &[arg::MODULE, arg::ONTO, arg::MIGRATE],
            LifecycleOp::Rollback => &[arg::MODULE, arg::POINT],
        }
    }

    /// Refuse any lifecycle argument member this operation does not apply. Silently
    /// accepting one would mislead the caller into believing a coordinate took
    /// effect, so it is rejected LOUDLY rather than dropped.
    fn reject_unsupported_args(args: &[(String, Value)], op: LifecycleOp) -> Result<(), Rejection> {
        let supported = Self::supported_args(op);
        for (key, _) in args {
            if !supported.contains(&key.as_str()) {
                return Err(Rejection::new(
                    RejectionReason::Malformed,
                    format!(
                        "`{}` does not support the `{key}` argument (§13.10/§13.16); \
                         supported members are {} — refusing rather than silently ignoring it",
                        op.member(),
                        supported.join(", "),
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Decode a package definition from `.liasse` bytes (§13.10). A malformed or
    /// incompatible package is a LOUD rejection that unwinds the whole transition.
    fn decode_bytes(bytes: &[u8]) -> Result<(String, DecodedPackageId), Rejection> {
        let decoded = decode_package_from_blob(bytes).map_err(|error| {
            Rejection::new(RejectionReason::Malformed, format!("malformed module package blob: {error} (§13.10)"))
        })?;
        let (definition, definition_id) = decoded.into_parts();
        Ok((
            definition,
            DecodedPackageId { definition: definition_id, content: liasse_value::Sha512::of(bytes) },
        ))
    }

    /// Decode the package definition from the `blob` argument's bytes (§13.10,
    /// §18.3): resolve the descriptor, fetch its bytes from the store, decode.
    fn decode(&self, args: &[(String, Value)], op: LifecycleOp) -> Result<(String, DecodedPackageId), Rejection> {
        let Some(Value::Blob(descriptor)) = args.iter().find(|(name, _)| name == "blob").map(|(_, value)| value) else {
            return Err(Rejection::new(
                RejectionReason::Malformed,
                format!("`module.{}` requires a `blob` argument carrying the package (§13.10)", op.member()),
            ));
        };
        Self::decode_bytes(&blob_bytes(self.blobs, descriptor)?)
    }

    /// The enabled instance a module handle addresses, or a LOUD refusal naming the
    /// mount that resolved to nothing — never a silently skipped operation.
    fn mounted(&self, at: &str, operator: &str) -> Result<&MountedInstance<'a, S>, Rejection> {
        self.instances
            .iter()
            .find(|instance| instance.at.render() == at && instance.enabled)
            .ok_or_else(|| {
                Rejection::new(
                    RejectionReason::Malformed,
                    format!(
                        "`{operator}` addresses `{at}`, which resolves to no enabled module \
                         instance in this transition (§13.10)"
                    ),
                )
            })
    }

    /// The engine of the enabled instance a module handle addresses.
    fn instance(&self, at: &str, operator: &str) -> Result<&Engine<S>, Rejection> {
        Ok(self.mounted(at, operator)?.engine)
    }

    /// The ADDRESS of the enabled instance a module handle addresses — the
    /// structured row address the intent carries, resolved from the live mount set
    /// rather than parsed out of the handle's rendering.
    fn address(&self, at: &str, operator: &str) -> Result<RowAddress, Rejection> {
        Ok(self.mounted(at, operator)?.at.clone())
    }

    /// The `.liasse` bytes a module operand carries: a pending handle's source blob,
    /// or a live instance's export.
    fn artifact_of(&self, operand: &ModuleOperand, operator: &str) -> Result<Vec<u8>, Rejection> {
        match operand {
            ModuleOperand::Pending(descriptor) => blob_bytes(self.blobs, descriptor),
            ModuleOperand::Mounted(at) => {
                let engine = self.instance(at, operator)?;
                engine.export().map_err(|error| {
                    Rejection::new(RejectionReason::Evaluation, format!("the instance could not be exported: {error}"))
                })
            }
        }
    }

    /// The selected point an artifact names, read from its verified manifest.
    fn artifact_point(bytes: &[u8], operator: &str) -> Result<HistoryPoint, Rejection> {
        Artifact::open(bytes)
            .map(|opened| opened.manifest().selected.clone())
            .map_err(|error| {
                Rejection::new(
                    RejectionReason::Malformed,
                    format!("`{operator}`'s artifact failed §19.8 verification: {error}"),
                )
            })
    }

    /// A refusal carrying an [`EngineError::Unsupported`] detail verbatim — the
    /// retention limits of §13.16 stated once, in `modules::value`, and surfaced
    /// here without being re-worded or softened.
    fn unsupported(error: EngineError) -> Rejection {
        Rejection::new(RejectionReason::Unsupported, error.to_string())
    }

    /// `pack(m, { model?, data?, history? })` (§13.16) — serialize a module's axes
    /// into a `.liasse` blob.
    ///
    /// A pending module IS its source blob, so `pack(unpack(b))` is `b` with nothing
    /// materialized (§13.16 laziness). A live instance packs through
    /// [`Engine::export`], the same §19.5 artifact `Engine::restore` and
    /// `Engine::classify` consume — so a packed module is a real, verifiable point
    /// and not a bespoke encoding. Every axis the instance does not retain is
    /// refused by name; see [`PackAxes::admit`].
    fn pack(&self, args: &[(String, Value)]) -> Result<Cell, Rejection> {
        let operator = LifecycleOp::Pack.member();
        let operand = ModuleOperand::read(args, arg::MODULE, operator)?;
        let axes = PackAxes::read(args);
        let descriptor = match &operand {
            ModuleOperand::Pending(descriptor) => {
                if !axes.is_current() {
                    return Err(Rejection::new(
                        RejectionReason::Unsupported,
                        "`pack` was given an axis coordinate over a module `unpack` has not yet \
                         materialized: it has no timeline, so it has no version, point or range \
                         other than the blob it carries (§13.16). Install it first, then pack the \
                         instance."
                            .to_owned(),
                    ));
                }
                // Materialization stays DEFERRED: the source blob is handed back
                // undecoded, exactly as `unpack` received it.
                descriptor.clone()
            }
            ModuleOperand::Mounted(at) => {
                let mounted = self.mounted(at, operator)?;
                let engine = mounted.engine;
                axes.admit(engine.package_version(), &engine.cursor().point())
                    .map_err(Self::unsupported)?;
                let bytes = engine.export().map_err(|error| {
                    Rejection::new(RejectionReason::Evaluation, format!("the instance could not be packed: {error}"))
                })?;
                let name = address::instance_name(mounted.at).unwrap_or_default();
                let descriptor = liasse_descriptor(&bytes, Some(format!("{name}.liasse")));
                self.intents.borrow_mut().push(LifecycleIntent::Pack { bytes });
                Box::new(descriptor)
            }
        };
        Ok(Cell::Scalar(Value::Blob(descriptor)))
    }

    /// `.modules[@id] <- m` (§13.16 "Install / override" and "Move") — move a module
    /// VALUE into a slot the interpreter has already resolved to a mount.
    ///
    /// The two operand shapes are the two halves of §13.16's move:
    ///
    /// - a **pending** module (`unpack(@package)`) has no instance yet, so it is
    ///   decoded and mounted through the very same [`LifecycleIntent::Install`] the
    ///   declarative `module.install` records — one runtime, two spellings;
    /// - a **mounted** module is already installed somewhere, so the move
    ///   *relocates* it. Within one module collection that is the §13.3 rekey,
    ///   which preserves the incarnation and therefore the durable identity. Into a
    ///   DIFFERENT collection it is not: that collection declares its own §13.4
    ///   parent surfaces, §13.5 peer set and §13.8 interface contracts, all of which
    ///   the instance was neither loaded under nor re-admitted against — so a bare
    ///   `<-` is REFUSED by name, and `reinstall_module(m)` is the explicit operator
    ///   that performs the re-admission. A silent rekey across a boundary that was
    ///   never checked stays unrepresentable.
    ///
    /// Either way the slot's occupant is dropped: §13.16 says a move into a slot
    /// replaces any instance already there.
    fn install_module(&self, at: RowAddress, args: &[(String, Value)]) -> Result<Cell, Rejection> {
        let operator = LifecycleOp::InstallModule.member();
        match ModuleOperand::read(args, arg::MODULE, operator)? {
            ModuleOperand::Pending(descriptor) => {
                let (definition, package) = Self::decode_bytes(&blob_bytes(self.blobs, &descriptor)?)?;
                let identity = Cell::Scalar(Value::Text(Text::new(package.definition.to_canonical_text())));
                self.intents.borrow_mut().push(LifecycleIntent::Install {
                    at,
                    definition,
                    package,
                    occupant: Occupant::Drop,
                });
                Ok(identity)
            }
            ModuleOperand::Mounted(source) => {
                let from = self.address(&source, operator)?;
                let identity = Cell::Scalar(Value::Text(Text::new(
                    address::instance_name(&at).unwrap_or_default().to_owned(),
                )));
                if from.collection() != at.collection() {
                    return Err(Rejection::new(
                        RejectionReason::Unsupported,
                        format!(
                            "`{operator}` would move `{source}` into `{}`, a DIFFERENT module \
                             collection: a module collection is a boundary, not a folder — it \
                             declares its own §13.4 parent surfaces, §13.5 peer set and §13.8 \
                             interface contracts, and this instance was admitted against the \
                             source's and against none of the destination's. Refused rather than \
                             re-keyed under a boundary it was never checked against. Write \
                             `<- reinstall_module(m)` to put it through that admission \
                             explicitly (§13.16).",
                            at.render(),
                        ),
                    ));
                }
                if from != at {
                    self.intents.borrow_mut().push(LifecycleIntent::Relocate {
                        from,
                        to: at,
                        occupant: Occupant::Drop,
                    });
                }
                Ok(identity)
            }
        }
    }

    /// `.other[@id] <- reinstall_module(m)` (§13.16) — move a mounted instance into
    /// a different module collection by RE-ADMITTING it there.
    ///
    /// This is the explicit form of the move `install_module` refuses. It performs
    /// no rekey: the host re-runs the destination's admission — the containing row
    /// must be live, the §13.5 peers must resolve in the destination's sibling set,
    /// the §13.4 parent surfaces must bind, and the instance's `$expose` must
    /// satisfy the destination's §13.8 interface contracts — and refuses the whole
    /// transition if any of them fails. The instance keeps its private store, data
    /// and history (its incarnation is its store identity); what changes is the
    /// boundary it is bound to, and that boundary is checked, never assumed.
    ///
    /// A pending module has no admission to re-run, so `reinstall_module(unpack(b))`
    /// is refused by name: the plain move already installs it.
    fn reinstall_module(&self, at: RowAddress, args: &[(String, Value)]) -> Result<Cell, Rejection> {
        let operator = LifecycleOp::Reinstall.member();
        let source = match ModuleOperand::read(args, arg::MODULE, operator)? {
            ModuleOperand::Mounted(source) => source,
            ModuleOperand::Pending(_) => {
                return Err(Rejection::new(
                    RejectionReason::Malformed,
                    format!(
                        "`{operator}` re-admits an instance that is already mounted, but its \
                         operand is a module `unpack` has not yet materialized: there is no \
                         admission to re-run. Move it in directly — a plain `<-` installs it \
                         (§13.16)."
                    ),
                ));
            }
        };
        let from = self.address(&source, operator)?;
        let identity =
            Cell::Scalar(Value::Text(Text::new(address::instance_name(&at).unwrap_or_default().to_owned())));
        if from == at {
            return Ok(identity);
        }
        self.intents.borrow_mut().push(LifecycleIntent::Reinstall { from, to: at, occupant: Occupant::Drop });
        Ok(identity)
    }

    /// `update_module(m, u, { migrate })` (§13.16) — apply the module `u` onto the
    /// live instance `m`, keeping `m`'s identity.
    ///
    /// `migrate: model` records the very same [`LifecycleIntent::Update`] the
    /// declarative `module.update` records, so it walks the §20.1 migration chain
    /// and carries `m`'s current data forward through one runtime.
    ///
    /// `migrate: model+data` additionally forwards `u`'s history and data, and the
    /// two histories reconcile by LINEAGE through [`Engine::classify`]: a
    /// fast-forward (or an already-synchronized point) applies automatically, and
    /// any divergence is refused with a structured [`AncestryDivergence`] — never
    /// merged, because §13.16 places that reconciliation outside the engine.
    fn update_module(&self, args: &[(String, Value)]) -> Result<Cell, Rejection> {
        let operator = LifecycleOp::UpdateModule.member();
        let target = ModuleOperand::read(args, arg::MODULE, operator)?;
        let mount = target.mount(operator)?;
        let onto = ModuleOperand::read(args, arg::ONTO, operator)?;
        let migrate = Self::migrate_axis(args)?;
        let artifact = self.artifact_of(&onto, operator)?;
        match migrate {
            MigrateAxis::Model => {
                let (definition, package) = Self::decode_bytes(&artifact)?;
                let identity = Cell::Scalar(Value::Text(Text::new(package.definition.to_canonical_text())));
                self.intents.borrow_mut().push(LifecycleIntent::Update {
                    at: self.address(mount, operator)?,
                    definition,
                    package,
                });
                Ok(identity)
            }
            MigrateAxis::ModelAndData => {
                let engine = self.instance(mount, operator)?;
                let relation = engine.classify(&artifact).map_err(|error| {
                    Rejection::new(
                        RejectionReason::Malformed,
                        format!("`{operator}`'s incoming module failed §19.8 verification: {error}"),
                    )
                })?;
                let incoming = Self::artifact_point(&artifact, operator)?;
                if !AncestryDivergence::is_fast_forward(relation) {
                    return Err(AncestryDivergence {
                        relation,
                        at: mount.to_owned(),
                        local: engine.cursor().point(),
                        incoming,
                    }
                    .into());
                }
                let identity = Cell::Scalar(Value::Text(Text::new(incoming.point().as_str())));
                if relation == ImportRelation::FastForward {
                    self.intents.borrow_mut().push(LifecycleIntent::Movement {
                        at: self.address(mount, operator)?,
                        artifact,
                        relation,
                    });
                }
                Ok(identity)
            }
        }
    }

    /// `rollback_module(m, @point)` (§13.16) — fork `m`'s timeline back to a
    /// retained point.
    ///
    /// The point is addressed by the `.liasse` artifact that CARRIES it — the one
    /// `pack` produced there — because a rollback needs the definition AND the state
    /// at the target, and only the artifact retains both. A bare instant is refused
    /// by name; see [`RollbackPoint::unsupported_instant`]. The movement itself is
    /// the existing §19.8 rollback: `Engine::import` under a `Rollback` policy,
    /// which selects the earlier point and leaves the cursor to branch a new lineage
    /// on the next commit — the fork §13.16 describes.
    fn rollback_module(&self, args: &[(String, Value)]) -> Result<Cell, Rejection> {
        let operator = LifecycleOp::Rollback.member();
        let target = ModuleOperand::read(args, arg::MODULE, operator)?;
        let mount = target.mount(operator)?;
        let engine = self.instance(mount, operator)?;
        let descriptor = match RollbackPoint::read(args)? {
            RollbackPoint::Instant(at) => {
                return Err(Self::unsupported(RollbackPoint::unsupported_instant(at, &engine.cursor().point())));
            }
            RollbackPoint::Artifact(descriptor) => descriptor,
        };
        let artifact = blob_bytes(self.blobs, &descriptor)?;
        let relation = engine.classify(&artifact).map_err(|error| {
            Rejection::new(
                RejectionReason::Malformed,
                format!("`{operator}`'s point artifact failed §19.8 verification: {error}"),
            )
        })?;
        let incoming = Self::artifact_point(&artifact, operator)?;
        let identity = Cell::Scalar(Value::Text(Text::new(incoming.point().as_str())));
        match relation {
            // Already at that point: a genuine no-op, not a silent skip.
            ImportRelation::SamePoint => Ok(identity),
            ImportRelation::Rollback => {
                self.intents.borrow_mut().push(LifecycleIntent::Movement {
                    at: self.address(mount, operator)?,
                    artifact,
                    relation,
                });
                Ok(identity)
            }
            other => Err(Rejection::new(
                RejectionReason::Unsupported,
                format!(
                    "`{operator}` forks BACK to a retained point, but the given artifact's point \
                     `{}/{}` does not precede the instance's live point `{}/{}` (§19.8 classifies \
                     it {other:?}). Refused rather than moving the instance somewhere it was \
                     never asked to go (§13.16).",
                    incoming.lineage().as_str(),
                    incoming.point().as_str(),
                    engine.cursor().point().lineage().as_str(),
                    engine.cursor().point().point().as_str(),
                ),
            )),
        }
    }

    /// The `migrate` axis, defaulting to `model` (§13.16). An unknown spelling is
    /// refused rather than falling back — the checker already rejects it at load, so
    /// this is the boundary's own defence against a caller that bypassed it.
    fn migrate_axis(args: &[(String, Value)]) -> Result<MigrateAxis, Rejection> {
        match args.iter().find(|(name, _)| name == arg::MIGRATE).map(|(_, value)| value) {
            None | Some(Value::None) => Ok(MigrateAxis::Model),
            Some(Value::Text(text)) => MigrateAxis::parse(text.as_str()).ok_or_else(|| {
                Rejection::new(
                    RejectionReason::Malformed,
                    format!(
                        "`update_module`'s `migrate` axis is one of {} (§13.16), not `{}`",
                        MigrateAxis::SPELLINGS.join(" or "),
                        text.as_str()
                    ),
                )
            }),
            Some(_) => Err(Rejection::new(
                RejectionReason::Malformed,
                format!(
                    "`update_module`'s `migrate` axis is one of {} (§13.16)",
                    MigrateAxis::SPELLINGS.join(" or ")
                ),
            )),
        }
    }
}

impl<S: InstanceStore> Lifecycle for LifecycleRecorder<'_, S> {
    fn perform(
        &self,
        op: LifecycleOp,
        at: Option<RowAddress>,
        args: Vec<(String, Value)>,
    ) -> Result<Cell, Rejection> {
        Self::reject_unsupported_args(&args, op)?;
        match op {
            // The declarative `module.install({ at: .modules[@n], blob: @pkg })`
            // addresses its slot the way every other write does — the interpreter
            // resolved `at` to an ordinary row address before this is reached.
            LifecycleOp::Install => {
                let at = Self::slot(at, op)?;
                let (definition, package) = self.decode(&args, op)?;
                // §5.1/§13.10: the decoded package identity is the intent's fact — the
                // caller reads it as the call's result (the D.4 identity text).
                let identity = Cell::Scalar(Value::Text(Text::new(package.definition.to_canonical_text())));
                self.intents
                    .borrow_mut()
                    .push(LifecycleIntent::Install { at, definition, package, occupant: Occupant::Refuse });
                Ok(identity)
            }
            // `module.update`/`module.remove` address the instance by the `module`
            // VALUE the caller holds (§13.16) — the same operand the value-surface
            // operators take, so one addressing serves both spellings.
            LifecycleOp::Update => {
                let target = ModuleOperand::read(&args, arg::MODULE, op.member())?;
                let at = self.address(target.mount(op.member())?, op.member())?;
                let (definition, package) = self.decode(&args, op)?;
                let identity = Cell::Scalar(Value::Text(Text::new(package.definition.to_canonical_text())));
                self.intents.borrow_mut().push(LifecycleIntent::Update { at, definition, package });
                Ok(identity)
            }
            LifecycleOp::Remove => {
                let target = ModuleOperand::read(&args, arg::MODULE, op.member())?;
                let at = self.address(target.mount(op.member())?, op.member())?;
                let identity =
                    Cell::Scalar(Value::Text(Text::new(address::instance_name(&at).unwrap_or_default().to_owned())));
                self.intents.borrow_mut().push(LifecycleIntent::Remove { at });
                Ok(identity)
            }
            LifecycleOp::InstallModule => self.install_module(Self::slot(at, op)?, &args),
            LifecycleOp::Reinstall => self.reinstall_module(Self::slot(at, op)?, &args),
            LifecycleOp::Pack => self.pack(&args),
            LifecycleOp::UpdateModule => self.update_module(&args),
            LifecycleOp::Rollback => self.rollback_module(&args),
        }
    }
}

impl<S: InstanceStore> LifecycleRecorder<'_, S> {
    /// The module-collection entry a slot-addressed operation writes. An operation
    /// that reached the handle without one is an interpreter contract breach: it is
    /// reported as such rather than defaulted to some entry.
    fn slot(at: Option<RowAddress>, op: LifecycleOp) -> Result<RowAddress, Rejection> {
        at.ok_or_else(|| {
            Rejection::new(
                RejectionReason::Malformed,
                format!(
                    "`{}` writes one entry of a module collection, but the call addressed no slot \
                     (§13.2/§13.16)",
                    op.member()
                ),
            )
        })
    }
}
