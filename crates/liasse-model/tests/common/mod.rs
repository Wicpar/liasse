//! Shared test harness: build a package definition string into a [`Model`] and
//! inspect the resulting diagnostics. Each test states the exact definition its
//! expectation is derived from, so nothing is tautological.

// Tests are expected to panic on a failed assertion (AGENTS.md), which the
// workspace deny-lints otherwise forbid.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use liasse_diag::{Diagnostics, SourceMap};
use liasse_model::Model;
use liasse_syntax::parse_document;

/// A build attempt with its source map retained for span inspection.
pub struct Built {
    pub sources: SourceMap,
    pub result: Result<Model, Diagnostics>,
}

/// Build a package definition from its Hjson/JSON authoring text.
pub fn build(definition: &str) -> Built {
    let mut sources = SourceMap::new();
    let id = sources.add_file("test.liasse", definition);
    let document = match parse_document(id, definition) {
        Ok(document) => document,
        Err(diags) => {
            return Built {
                sources,
                result: Err(diags),
            };
        }
    };
    let result = Model::build(&mut sources, id, &document);
    Built { sources, result }
}

impl Built {
    /// The model, panicking with the rendered diagnostics if the build failed.
    pub fn expect_ok(&self) -> &Model {
        match &self.result {
            Ok(model) => model,
            Err(diags) => panic!(
                "expected the package to load, but it was rejected:\n{}",
                diags.render(&self.sources)
            ),
        }
    }

    /// The diagnostics, panicking if the build unexpectedly succeeded.
    pub fn expect_err(&self) -> &Diagnostics {
        match &self.result {
            Ok(_) => panic!("expected the package to be rejected, but it loaded"),
            Err(diags) => diags,
        }
    }

    /// The error codes present, in order.
    pub fn codes(&self) -> Vec<String> {
        self.expect_err()
            .iter()
            .filter_map(|d| d.code().map(|c| c.as_str().to_owned()))
            .collect()
    }

    /// Whether any rejection carries `code`.
    pub fn has_code(&self, code: &str) -> bool {
        self.codes().iter().any(|c| c == code)
    }

    /// The source text under each diagnostic's primary span, so a test can
    /// assert the diagnostic points at the offending bytes.
    pub fn primary_spans(&self) -> Vec<String> {
        self.expect_err()
            .iter()
            .filter_map(|d| self.sources.span_text(d.primary().span()).map(str::to_owned))
            .collect()
    }

    /// Whether some rejection points at source text containing `needle`.
    pub fn points_at(&self, needle: &str) -> bool {
        self.primary_spans().iter().any(|text| text.contains(needle))
    }

    /// Whether any rejection carries at least one fix hint.
    pub fn has_hint(&self) -> bool {
        self.expect_err().iter().any(|d| !d.helps().is_empty())
    }

    /// The full rendered diagnostics, for debugging a failing test.
    pub fn rendered(&self) -> String {
        self.expect_err().render(&self.sources)
    }
}
