//! The single error type for fallible identity, path, and digest parsing.

use thiserror::Error;

/// Every way a raw identity/path/digest input can fail to parse into a
/// well-formed [`crate`] type. Past a successful parse the value is proof of
/// conformance and nothing downstream re-checks it.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IdentError {
    /// A [`liasse_value::Value`] variant that Annex D.2 does not give a
    /// canonical scalar key text (e.g. `period`, `json`, `blob`, `set`, `map`,
    /// `none`) was offered as a key component.
    #[error("value of type `{type_name}` is not a scalar key component (D.2)")]
    NotKeyComponent { type_name: &'static str },

    /// A key was built from zero components; a Liasse key always names at least
    /// one field (§5.4).
    #[error("a key must have at least one component")]
    EmptyKey,

    /// A key value flattened to an empty canonical component — an empty `text`,
    /// or an empty `bytes` (base64 `""`). An empty component makes a display
    /// path non-injective / non-round-trippable, so it is inadmissible
    /// (A.8/D.2/D.3, SPEC-ISSUES item 31).
    #[error("a key component is the empty canonical value; an empty key component is not admissible (A.8/D.2)")]
    EmptyKeyComponent,

    /// A percent escape was not one of the canonical D.2/D.3 sequences
    /// (`%25`, `%2F`, or — in a key segment — `%3A`).
    #[error("malformed percent escape in `{text}` (canonical escapes are %25, %2F, %3A)")]
    MalformedEscape { text: String },

    /// A display path did not begin with the root separator `/` (D.3).
    #[error("display path must begin with `/`: `{text}`")]
    PathMissingRoot { text: String },

    /// A display path contained an empty segment (a `//` or trailing `/`).
    #[error("display path has an empty segment: `{text}`")]
    EmptyPathSegment { text: String },

    /// A digest text was not the canonical `sha256:<64 lowercase hex>` form
    /// (D.4/D.5/D.7).
    #[error("malformed digest text: {detail}")]
    MalformedDigest { detail: String },
}
