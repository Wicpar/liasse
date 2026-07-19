//! Package compatibility, version layer (SPEC.md Annex E, §20.3).
//!
//! Annex E defines compatibility at boundary *contracts* (surfaces, view and
//! mutation shapes, authenticators, interface bindings, capabilities). Deciding
//! whether one release's effective contracts narrow another's needs the typed
//! model and lives in `liasse-model`/the runtime. What this crate owns is the
//! **version relationship** the checker runs *within* — the E.1 rule that keys
//! everything else:
//!
//! ```text
//! major    may change or remove boundary contracts
//! minor    may add or widen compatible boundary contracts
//! patch    preserves the same boundary contracts, correcting their implementation
//! ```
//!
//! [`CompatibilityDecision::classify`] turns an active identity and a candidate
//! identity into an [`UpdateRelation`] and the [`ContractRule`] the runtime must
//! then apply to the effective contracts. It is deliberately a *decision the
//! runtime consumes*, not a verdict: version arithmetic alone never proves a
//! release compatible (arbitrary expression equivalence is undecidable, E.3), it
//! only says which rule governs.

use crate::version::PackageIdentity;

/// How a candidate version relates to the active one on the same line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateRelation {
    /// Identical `name@version`. A same-version republish is a §9.3
    /// definition-only update under the chosen §9.2 action, gated by the same
    /// non-narrowing check as a forward release (Annex E.1, item 22 pinned).
    SameVersion,
    /// Same major and minor, higher patch (E.1 patch release).
    Patch,
    /// Same major, higher minor (E.1 minor release), patch unconstrained.
    Minor,
    /// Higher major (E.1 major release): contracts may break.
    Major,
    /// A lower version on any major: a downgrade (§20.2).
    Downgrade,
    /// A different compatibility line (different package name); the versions are
    /// not comparable and an unrelated-install policy governs (§19.8).
    Unrelated,
}

/// Which Annex E rule the runtime must apply to the effective boundary
/// contracts once the version relationship is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractRule {
    /// Within one major, forward: every prospective boundary contract MUST
    /// preserve or widen the active one (E.1, E.4, E.5). The runtime rejects any
    /// narrowing it can establish (E.3). The registry additionally checks the
    /// prospective contract does not narrow relative to *any* earlier release in
    /// the major, not only the active one (E.1) — that needs the release
    /// history, which this per-pair decision does not carry.
    MustPreserveOrWiden,
    /// A new major: boundary contracts MAY change or be removed (E.1). The
    /// update still validates the prospective model, migrations, interfaces, and
    /// retained history (§20.3) and requires a migration when owned state must be
    /// transformed (§20.1).
    MayBreak,
    /// A downgrade: the down-direction walk of the §20.1 route. The §20.1
    /// compatible copy applies as on an upgrade, so a downgrade that loses no
    /// live value commits with no explicit transform; it is rejected only when
    /// a populated live value cannot be represented and no declared or deduced
    /// inverse preserves it (§20.2, item 22 pinned).
    RequiresDowngradeTransform,
    /// A same-version republish: a §9.3 definition-only update under the chosen
    /// §9.2 action, subject to the same non-narrowing gate as a forward release
    /// (Annex E.1, item 22 pinned).
    SameLine,
    /// A different compatibility line: no substitutability promise exists; an
    /// unrelated import/install policy decides (§19.8).
    Unrelated,
}

/// The version-layer compatibility decision for an update from `active` to
/// `candidate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompatibilityDecision {
    /// The version relationship.
    pub relation: UpdateRelation,
    /// The contract rule the runtime must apply.
    pub rule: ContractRule,
}

impl CompatibilityDecision {
    /// Classify an update from the `active` package identity to the `candidate`.
    #[must_use]
    pub fn classify(active: &PackageIdentity, candidate: &PackageIdentity) -> Self {
        if active.name != candidate.name {
            return Self {
                relation: UpdateRelation::Unrelated,
                rule: ContractRule::Unrelated,
            };
        }
        let (a, c) = (active.version, candidate.version);
        let (relation, rule) = if c == a {
            (UpdateRelation::SameVersion, ContractRule::SameLine)
        } else if c < a {
            (UpdateRelation::Downgrade, ContractRule::RequiresDowngradeTransform)
        } else if c.major > a.major {
            (UpdateRelation::Major, ContractRule::MayBreak)
        } else if c.minor > a.minor {
            // Same major (c.major == a.major since c > a and not a higher major),
            // higher minor.
            (UpdateRelation::Minor, ContractRule::MustPreserveOrWiden)
        } else {
            // Same major and minor, higher patch.
            (UpdateRelation::Patch, ContractRule::MustPreserveOrWiden)
        };
        Self { relation, rule }
    }

    /// Whether this is a forward move on the same compatibility line that Annex
    /// E holds to preserve-or-widen (a minor or patch within one major). The
    /// mechanical narrowing check (E.3) applies exactly to these.
    #[must_use]
    pub fn is_line_forward(&self) -> bool {
        matches!(self.rule, ContractRule::MustPreserveOrWiden)
    }

    /// Whether the E.3 mechanical non-narrowing gate applies: every same-line
    /// forward move, plus a same-version republish — Annex E.1 admits the latter
    /// as a definition-only update only when it does not narrow (item 22
    /// pinned), so a narrowing release cannot sneak in under a reused version.
    #[must_use]
    pub fn requires_non_narrowing(&self) -> bool {
        matches!(self.rule, ContractRule::MustPreserveOrWiden | ContractRule::SameLine)
    }
}
