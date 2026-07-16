//! Package identity and semantic versions (SPEC.md §2.5, §4.3, Annex E.1).
//!
//! A `$app`/`$module` value is `name@version`. The name is the compatibility
//! *line* (Annex E) and the version drives update compatibility. These are
//! parse-don't-validate boundaries: a constructed [`PackageIdentity`] is proof
//! the text passed the grammar, so [`crate::CompatibilityDecision`] can classify
//! any two of them without re-checking.
//!
//! This mirrors the grammar `liasse-model` enforces on the definition header;
//! the artifact layer keeps its own copy so package compatibility can be
//! reasoned about from a manifest alone, without pulling in the whole model.

use crate::error::ArtifactError;

/// A validated package name (§2.5): dot-separated components over `a`–`z`,
/// `0`–`9`, `_`, each beginning with a lowercase letter.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageName(String);

impl PackageName {
    /// Parse a package name, explaining any rejection.
    pub fn parse(text: &str) -> Result<Self, ArtifactError> {
        if text.is_empty() {
            return Err(reject("a package name must not be empty"));
        }
        for component in text.split('.') {
            Self::check_component(text, component)?;
        }
        Ok(Self(text.to_owned()))
    }

    fn check_component(full: &str, component: &str) -> Result<(), ArtifactError> {
        let mut chars = component.chars();
        match chars.next() {
            None => Err(reject(format!(
                "package name `{full}` has an empty dot-separated component"
            ))),
            Some(first) if !first.is_ascii_lowercase() => Err(reject(format!(
                "package-name component `{component}` must begin with a lowercase letter"
            ))),
            Some(_) => {
                for ch in chars {
                    if !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_') {
                        return Err(reject(format!(
                            "package-name component `{component}` contains `{ch}`; only lowercase letters, digits, and `_` are allowed"
                        )));
                    }
                }
                Ok(())
            }
        }
    }

    /// The name text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A semantic version (§4.3, Annex E.1): `major.minor.patch`, each a base-10
/// integer. Prerelease and build metadata are unspecified in v0.5 (SPEC-ISSUES
/// item 26); pending resolution the strict three-component reading is taken so
/// a version can always participate in the Annex E algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// May change or remove boundary contracts (E.1).
    pub major: u64,
    /// May add or widen compatible boundary contracts (E.1).
    pub minor: u64,
    /// Preserves boundary contracts, correcting their implementation (E.1).
    pub patch: u64,
}

impl Version {
    /// Construct a version directly (all three components known).
    #[must_use]
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Parse a `major.minor.patch` version, explaining any rejection.
    pub fn parse(text: &str) -> Result<Self, ArtifactError> {
        let mut parts = text.split('.');
        let major = Self::component(text, parts.next())?;
        let minor = Self::component(text, parts.next())?;
        let patch = Self::component(text, parts.next())?;
        if parts.next().is_some() {
            return Err(reject(format!(
                "`{text}` has more than the three components of a `major.minor.patch` version"
            )));
        }
        Ok(Self {
            major,
            minor,
            patch,
        })
    }

    fn component(full: &str, part: Option<&str>) -> Result<u64, ArtifactError> {
        let part = part
            .ok_or_else(|| reject(format!("`{full}` is not a `major.minor.patch` semantic version")))?;
        if part.is_empty() {
            return Err(reject(format!("`{full}` has an empty version component")));
        }
        part.parse::<u64>()
            .map_err(|_| reject(format!("version component `{part}` in `{full}` is not a base-10 integer")))
    }
}

/// A `name@version` package identity as written in `$app` / `$module`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PackageIdentity {
    /// The compatibility-line name.
    pub name: PackageName,
    /// The definition version.
    pub version: Version,
}

impl PackageIdentity {
    /// Bind a name and version directly.
    #[must_use]
    pub const fn new(name: PackageName, version: Version) -> Self {
        Self { name, version }
    }

    /// Parse a `name@version` identity, explaining any rejection.
    pub fn parse(text: &str) -> Result<Self, ArtifactError> {
        let (name, version) = text
            .split_once('@')
            .ok_or_else(|| reject(format!("package identity `{text}` must be written as `name@version`")))?;
        Ok(Self {
            name: PackageName::parse(name)?,
            version: Version::parse(version)?,
        })
    }

    /// The canonical `name@version` text.
    #[must_use]
    pub fn to_canonical_text(&self) -> String {
        let Version {
            major,
            minor,
            patch,
        } = self.version;
        format!("{}@{major}.{minor}.{patch}", self.name.as_str())
    }
}

fn reject(detail: impl Into<String>) -> ArtifactError {
    ArtifactError::PackageIdentity {
        detail: detail.into(),
    }
}
