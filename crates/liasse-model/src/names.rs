//! Name and version grammars (SPEC.md §2.5, §4.3, Annex E.1).
//!
//! These are parse-don't-validate boundaries: a [`DeclName`], [`PackageName`],
//! or [`Version`] exists only once its text has passed the grammar, so every
//! downstream use is proof the text was well-formed. The fallible constructors
//! return a human-readable reason on rejection; the caller owns the span and
//! turns the reason into a diagnostic.

/// Whether `name` is a reserved Liasse structural member (begins with `$`).
#[must_use]
pub(crate) fn is_reserved(name: &str) -> bool {
    name.starts_with('$')
}

/// A validated application declaration name (§2.5): begins with an ASCII
/// letter, then ASCII letters, digits, and `_`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeclName(String);

impl DeclName {
    /// Parse a declaration name, or explain why it is rejected.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let mut chars = text.chars();
        match chars.next() {
            None => return Err("a declaration name must not be empty".to_owned()),
            Some(first) if !first.is_ascii_alphabetic() => {
                return Err(format!(
                    "declaration name `{text}` must begin with an ASCII letter"
                ));
            }
            Some(_) => {}
        }
        for ch in chars {
            if !(ch.is_ascii_alphanumeric() || ch == '_') {
                return Err(format!(
                    "declaration name `{text}` contains `{ch}`; only ASCII letters, digits, and `_` are allowed"
                ));
            }
        }
        Ok(Self(text.to_owned()))
    }

    /// The name text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A validated package name (§2.5): dot-separated lowercase identifiers over
/// `a`–`z`, `0`–`9`, `_`, each component beginning with a letter.
///
/// A single-component name (no dot) is well-formed (§2.5, SPEC-ISSUES item 26):
/// any minimum component count is registry policy, not a grammar constraint, so
/// this parser imposes no minimum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageName(String);

impl PackageName {
    /// Parse a package name, or explain the rejection.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        if text.is_empty() {
            return Err("a package name must not be empty".to_owned());
        }
        for component in text.split('.') {
            Self::check_component(text, component)?;
        }
        Ok(Self(text.to_owned()))
    }

    fn check_component(full: &str, component: &str) -> Result<(), String> {
        let mut chars = component.chars();
        match chars.next() {
            None => Err(format!(
                "package name `{full}` has an empty dot-separated component"
            )),
            Some(first) if !first.is_ascii_lowercase() => Err(format!(
                "package-name component `{component}` must begin with a lowercase letter"
            )),
            Some(_) => {
                for ch in chars {
                    if !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_') {
                        return Err(format!(
                            "package-name component `{component}` contains `{ch}`; only lowercase letters, digits, and `_` are allowed"
                        ));
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
/// integer.
///
/// A package version is exactly the three numeric components (§4.3, Annex E.1,
/// SPEC-ISSUES item 26): pre-release identifiers (`1.0.0-rc.1`) and build
/// metadata (`1.0.0+build`) are rejected, since the Annex E compatibility
/// algorithm assigns roles to `major`/`minor`/`patch` alone and no pre-release
/// may alias its own final release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    /// May change or remove boundary contracts (E.1).
    pub major: u64,
    /// May add or widen compatible boundary contracts (E.1).
    pub minor: u64,
    /// Preserves boundary contracts (E.1).
    pub patch: u64,
}

impl Version {
    /// Parse a `major.minor.patch` version, or explain the rejection.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let mut parts = text.split('.');
        let major = Self::component(text, parts.next())?;
        let minor = Self::component(text, parts.next())?;
        let patch = Self::component(text, parts.next())?;
        if parts.next().is_some() {
            return Err(format!(
                "`{text}` has more than the three components of a `major.minor.patch` version"
            ));
        }
        Ok(Self {
            major,
            minor,
            patch,
        })
    }

    fn component(full: &str, part: Option<&str>) -> Result<u64, String> {
        let part = part.ok_or_else(|| {
            format!("`{full}` is not a `major.minor.patch` semantic version")
        })?;
        if part.is_empty() {
            return Err(format!("`{full}` has an empty version component"));
        }
        part.parse::<u64>().map_err(|_| {
            format!("version component `{part}` in `{full}` is not a base-10 integer")
        })
    }
}

/// A package identity: a compatibility-line name and a semantic version, as
/// written in `$app` / `$module` (`name@version`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageId {
    /// The compatibility-line name.
    pub name: PackageName,
    /// The definition version.
    pub version: Version,
}

impl PackageId {
    /// Parse a `name@version` identity, or explain the rejection.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let (name, version) = text.split_once('@').ok_or_else(|| {
            format!("package identity `{text}` must be written as `name@version`")
        })?;
        Ok(Self {
            name: PackageName::parse(name)?,
            version: Version::parse(version)?,
        })
    }
}
