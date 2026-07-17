//! `enum` — one declared label (A.1). Ordering (B.1) is declaration order.

use core::cmp::Ordering;

use crate::error::ValueError;

/// A declared enum type: an ordered, unique list of labels. Declaration order
/// is the sort order (B.1), so it is carried explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumType {
    labels: Vec<String>,
}

impl EnumType {
    /// Build from declared labels in declaration order. Duplicate labels are
    /// rejected: an enum's identity is its distinct label set.
    pub fn new(labels: impl IntoIterator<Item = String>) -> Result<Self, ValueError> {
        let labels: Vec<String> = labels.into_iter().collect();
        for (index, label) in labels.iter().enumerate() {
            if labels.get(..index).is_some_and(|prior| prior.contains(label)) {
                return Err(ValueError::UnknownEnumLabel {
                    label: label.clone(),
                    allowed: labels.clone(),
                });
            }
        }
        Ok(Self { labels })
    }

    /// The declared labels in declaration order.
    #[must_use]
    pub fn labels(&self) -> &[String] {
        &self.labels
    }

    /// Parse a wire label into a positioned [`EnumValue`].
    pub fn parse(&self, label: &str) -> Result<EnumValue, ValueError> {
        let ordinal = self
            .labels
            .iter()
            .position(|declared| declared == label)
            .ok_or_else(|| ValueError::UnknownEnumLabel {
                label: label.to_owned(),
                allowed: self.labels.clone(),
            })?;
        Ok(EnumValue {
            ordinal: ordinal as u32,
            label: label.to_owned(),
        })
    }
}

/// One enum value: its label (the wire form, A.1) plus its declaration-order
/// position. Within a column the ordinal is the B.1 comparison key; the label
/// is retained so equality and ordering stay well defined across unrelated
/// declarations that happen to share an ordinal.
#[derive(Debug, Clone)]
pub struct EnumValue {
    ordinal: u32,
    label: String,
}

impl EnumValue {
    /// Reconstruct an enum value directly from its `(ordinal, label)` parts.
    ///
    /// An `EnumValue` *is* the `(ordinal, label)` pair — the declaration-order
    /// position and the label text on which its [`PartialEq`] and Annex B [`Ord`]
    /// are defined, and nothing else. A schema-free store that durably recorded
    /// exactly this pair reconstructs the value from it directly, without the
    /// originating [`EnumType`]: this is the faithful inverse of reading
    /// [`ordinal`](Self::ordinal) and [`label`](Self::label) back out. A label is
    /// arbitrary A.1 text (including `U+0000`), so reconstruction must not route
    /// through a fabricated declaration whose synthetic labels could collide with
    /// it.
    #[must_use]
    pub fn from_parts(ordinal: u32, label: impl Into<String>) -> Self {
        Self { ordinal, label: label.into() }
    }

    /// The label string (A.1 wire / D.2 key text).
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The declaration-order position.
    #[must_use]
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }
}

impl PartialEq for EnumValue {
    fn eq(&self, other: &Self) -> bool {
        // Two values are the same enum value only when both their declared
        // position *and* their label agree. Comparing the ordinal alone would
        // conflate the Nth label of two unrelated enum declarations.
        self.ordinal == other.ordinal && self.label == other.label
    }
}

impl Eq for EnumValue {}

impl PartialOrd for EnumValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for EnumValue {
    fn cmp(&self, other: &Self) -> Ordering {
        // B.1: an enum column orders by declaration order (ordinal). A sort
        // column is single-typed, so Annex B never compares values from two
        // different enum declarations; `Ord` nonetheless demands a total order,
        // so the label breaks any cross-declaration tie deterministically.
        // Within one declaration ordinal↔label is a bijection (labels are
        // unique, `EnumType::new`), so the label tiebreak never perturbs the
        // declared within-column order.
        self.ordinal
            .cmp(&other.ordinal)
            .then_with(|| self.label.cmp(&other.label))
    }
}
