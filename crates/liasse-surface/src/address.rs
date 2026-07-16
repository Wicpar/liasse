//! Dotted external addresses (SPEC.md §10, §12.1).
//!
//! A client names a surface by a dotted path. The corpus fixes the form
//! (`tests/10-interfaces-roles/NOTES.md`):
//!
//! - `public.<surface>` / `public.<surface>.<call>` — a public surface (§10.2);
//! - `<role>.<surface>` / `<role>.<surface>.<call>` — a scoped-role surface
//!   (§10.3), the leading segment being the role the request names (§11.4).
//!
//! A two-segment address targets the surface's `$view`; a three-segment address
//! targets one `$mut` call. Segments are the exact declared names — no folding,
//! trimming, or Unicode normalization (§2.5 names are ASCII), so a confusable or
//! case-variant name is simply a different, unresolvable address.

/// The authority a dotted address is scoped under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authority {
    /// A `$public` surface, callable with no actor (§10.2).
    Public,
    /// A `$roles` surface, named by its role (§10.3).
    Role(String),
}

/// A parsed external surface address: its authority, the surface name, and the
/// optional call name (absent for a view target).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceAddress {
    authority: Authority,
    surface: String,
    call: Option<String>,
}

/// The reserved leading segment selecting the public authority.
const PUBLIC: &str = "public";

impl SurfaceAddress {
    /// Parse a dotted address into its segments. An address with fewer than two
    /// or more than three segments, or an empty segment, is malformed.
    ///
    /// Malformedness is a property of the wire text alone; it is deliberately
    /// distinct from an address that parses but resolves to nothing exposed
    /// (that is a resolution-time denial, §10.1/§12.1).
    pub fn parse(text: &str) -> Result<Self, AddressError> {
        let segments: Vec<&str> = text.split('.').collect();
        if segments.iter().any(|segment| segment.is_empty()) {
            return Err(AddressError::EmptySegment);
        }
        match segments.as_slice() {
            [authority, surface] => {
                Ok(Self { authority: Self::authority_of(authority), surface: (*surface).to_owned(), call: None })
            }
            [authority, surface, call] => Ok(Self {
                authority: Self::authority_of(authority),
                surface: (*surface).to_owned(),
                call: Some((*call).to_owned()),
            }),
            [_] => Err(AddressError::MissingSurface),
            _ => Err(AddressError::TooManySegments),
        }
    }

    /// The authority a leading segment names: the reserved `public`, else a role.
    fn authority_of(segment: &str) -> Authority {
        if segment == PUBLIC {
            Authority::Public
        } else {
            Authority::Role(segment.to_owned())
        }
    }

    /// The authority the address is scoped under.
    #[must_use]
    pub fn authority(&self) -> &Authority {
        &self.authority
    }

    /// The role name when the address is role-scoped.
    #[must_use]
    pub fn role(&self) -> Option<&str> {
        match &self.authority {
            Authority::Role(role) => Some(role),
            Authority::Public => None,
        }
    }

    /// The surface name.
    #[must_use]
    pub fn surface(&self) -> &str {
        &self.surface
    }

    /// The call name, or `None` for a view target.
    #[must_use]
    pub fn call(&self) -> Option<&str> {
        self.call.as_deref()
    }

    /// Whether this address targets a mutation call (three segments) rather than
    /// a view (two segments).
    #[must_use]
    pub fn is_call(&self) -> bool {
        self.call.is_some()
    }

    /// The canonical `public.<surface>` / `<role>.<surface>` surface prefix,
    /// without any call segment — the manifest and operation-scope form.
    #[must_use]
    pub fn surface_prefix(&self) -> String {
        match &self.authority {
            Authority::Public => format!("{PUBLIC}.{}", self.surface),
            Authority::Role(role) => format!("{role}.{}", self.surface),
        }
    }
}

/// Why a dotted address failed to parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AddressError {
    /// A segment between dots was empty, including the empty address (`public..add`, `""`).
    #[error("an address segment is empty")]
    EmptySegment,
    /// Only an authority segment was present, with no surface.
    #[error("an address needs a surface name")]
    MissingSurface,
    /// More than `authority.surface.call` segments were present.
    #[error("an address has at most three segments")]
    TooManySegments,
}
