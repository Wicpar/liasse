//! [`Diagnostics`]: an ordered bag of diagnostics for accumulate-then-report
//! flows. A parser emits into it as it goes, then the caller asks whether any
//! errors accumulated before rendering the whole batch.

use crate::diagnostic::Diagnostic;

/// An ordered collection of diagnostics.
///
/// Push diagnostics as they are found, in source order; query [`has_errors`]
/// to decide whether the operation failed, then render the batch.
///
/// [`has_errors`]: Diagnostics::has_errors
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
}

impl Diagnostics {
    /// An empty collection.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one diagnostic, preserving insertion order.
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.items.push(diagnostic);
    }

    /// Whether the collection holds no diagnostics at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The total number of diagnostics of every severity.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether any diagnostic has [`Severity::Error`](crate::Severity::Error) —
    /// the signal that the accumulated operation failed.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.items.iter().any(Diagnostic::is_error)
    }

    /// How many diagnostics are errors.
    #[must_use]
    pub fn error_count(&self) -> usize {
        self.items.iter().filter(|d| d.is_error()).count()
    }

    /// The diagnostics in insertion order.
    pub fn iter(&self) -> core::slice::Iter<'_, Diagnostic> {
        self.items.iter()
    }
}

impl Extend<Diagnostic> for Diagnostics {
    fn extend<T: IntoIterator<Item = Diagnostic>>(&mut self, iter: T) {
        self.items.extend(iter);
    }
}

impl FromIterator<Diagnostic> for Diagnostics {
    fn from_iter<T: IntoIterator<Item = Diagnostic>>(iter: T) -> Self {
        Self {
            items: iter.into_iter().collect(),
        }
    }
}

impl IntoIterator for Diagnostics {
    type Item = Diagnostic;
    type IntoIter = std::vec::IntoIter<Diagnostic>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

impl<'a> IntoIterator for &'a Diagnostics {
    type Item = &'a Diagnostic;
    type IntoIter = core::slice::Iter<'a, Diagnostic>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}
