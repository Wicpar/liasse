//! Contract identity and the Annex E version-acceptance rule for host
//! namespaces (§16.2, Annex E.1/E.8).
//!
//! A `$requires` value names a semantic contract and a *compatible major*
//! ([`ContractRef`], `name@major`). A registered descriptor carries a full
//! [`Version`] (`major.minor.patch`). Acceptance follows E.8: a requirement MAY
//! resolve to any descriptor of the same contract within the same major.

use std::fmt;

use thiserror::Error;

/// A validated namespace contract name (§2.5 package-name grammar):
/// dot-separated lowercase identifiers over `a`–`z`, `0`–`9`, `_`, each
/// component beginning with a letter. This names the *semantic contract*
/// (`liasse.cbor`), not the local `$requires` key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContractName(String);

impl ContractName {
    /// Parse a contract name, or explain why it is not one.
    pub fn parse(text: &str) -> Result<Self, ContractError> {
        if text.is_empty() {
            return Err(ContractError::EmptyName);
        }
        for component in text.split('.') {
            let mut chars = component.chars();
            match chars.next() {
                None => return Err(ContractError::EmptyComponent(text.to_owned())),
                Some(first) if !first.is_ascii_lowercase() => {
                    return Err(ContractError::BadFirstChar {
                        component: component.to_owned(),
                    });
                }
                Some(_) => {
                    for ch in chars {
                        if !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_') {
                            return Err(ContractError::BadChar {
                                component: component.to_owned(),
                                found: ch,
                            });
                        }
                    }
                }
            }
        }
        Ok(Self(text.to_owned()))
    }

    /// The contract-name text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContractName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A semantic version (Annex E.1): `major.minor.patch`, each a base-10 integer.
/// Ordering is the natural component order, so E.8 "compatible minor or patch"
/// resolution can pick the newest within a major.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// May change or remove boundary contracts (E.1).
    pub major: u64,
    /// May add or widen compatible boundary contracts (E.1).
    pub minor: u64,
    /// Preserves boundary contracts, correcting implementation (E.1).
    pub patch: u64,
}

impl Version {
    /// A version from its three components.
    #[must_use]
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Parse a `major.minor.patch` version, or explain the rejection.
    pub fn parse(text: &str) -> Result<Self, ContractError> {
        let mut parts = text.split('.');
        let major = Self::component(text, parts.next())?;
        let minor = Self::component(text, parts.next())?;
        let patch = Self::component(text, parts.next())?;
        if parts.next().is_some() {
            return Err(ContractError::MalformedVersion(text.to_owned()));
        }
        Ok(Self {
            major,
            minor,
            patch,
        })
    }

    fn component(full: &str, part: Option<&str>) -> Result<u64, ContractError> {
        let part = part.ok_or_else(|| ContractError::MalformedVersion(full.to_owned()))?;
        part.parse::<u64>()
            .map_err(|_| ContractError::MalformedVersion(full.to_owned()))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A `$requires` reference: a contract name and the compatible major it pins
/// (`name@major`, §16.2). The local `$requires` key (the expression namespace)
/// is not part of this value — it is chosen by the requiring package.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContractRef {
    name: ContractName,
    major: u64,
}

impl ContractRef {
    /// Build a reference from a parsed name and a major.
    #[must_use]
    pub fn new(name: ContractName, major: u64) -> Self {
        Self { name, major }
    }

    /// Parse a `name@major` reference. A missing or non-integer major is
    /// rejected (a `$requires` value MUST pin a compatible major, §16.2).
    pub fn parse(text: &str) -> Result<Self, ContractError> {
        let (name, major) = text
            .split_once('@')
            .ok_or_else(|| ContractError::MissingMajor(text.to_owned()))?;
        let name = ContractName::parse(name)?;
        let major = major
            .parse::<u64>()
            .map_err(|_| ContractError::MalformedMajor(major.to_owned()))?;
        Ok(Self { name, major })
    }

    /// The required contract name.
    #[must_use]
    pub fn name(&self) -> &ContractName {
        &self.name
    }

    /// The required compatible major.
    #[must_use]
    pub const fn major(&self) -> u64 {
        self.major
    }

    /// Whether a descriptor of `id` at `version` satisfies this requirement:
    /// same contract, same major (Annex E.8 — minor/patch within a major is a
    /// compatible substitution).
    #[must_use]
    pub fn accepts(&self, id: &ContractName, version: Version) -> bool {
        &self.name == id && version.major == self.major
    }
}

impl fmt::Display for ContractRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.major)
    }
}

/// Every way a contract identity fails to parse.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ContractError {
    /// The contract name was empty.
    #[error("a contract name must not be empty")]
    EmptyName,
    /// A dot-separated component was empty.
    #[error("contract name `{0}` has an empty dot-separated component")]
    EmptyComponent(String),
    /// A component did not begin with a lowercase letter.
    #[error("contract-name component `{component}` must begin with a lowercase letter")]
    BadFirstChar {
        /// The offending component.
        component: String,
    },
    /// A component held a disallowed character.
    #[error("contract-name component `{component}` contains `{found}`; only lowercase letters, digits, and `_` are allowed")]
    BadChar {
        /// The offending component.
        component: String,
        /// The disallowed character.
        found: char,
    },
    /// A `$requires` value carried no `@major` suffix.
    #[error("requirement `{0}` must be written as `name@major`")]
    MissingMajor(String),
    /// The `@major` suffix was not a base-10 integer.
    #[error("requirement major `{0}` is not a base-10 integer")]
    MalformedMajor(String),
    /// A descriptor version was not `major.minor.patch`.
    #[error("`{0}` is not a `major.minor.patch` version")]
    MalformedVersion(String),
}
