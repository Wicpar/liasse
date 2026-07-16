//! Loader error type. Every error names the offending file and, where the
//! failure is a shape violation, the field path within the case.

use std::path::{Path, PathBuf};

use thiserror::Error;

/// A failure to turn a corpus file into a typed [`crate::Case`].
#[derive(Debug, Error)]
pub enum LoadError {
    /// The file could not be read from disk.
    #[error("{path}: cannot read file: {source}")]
    Read {
        /// Absolute path of the unreadable file.
        path: PathBuf,
        /// Underlying IO error.
        source: std::io::Error,
    },

    /// The file is not well-formed Hjson.
    #[error("{path}: Hjson syntax error: {message}")]
    Syntax {
        /// Absolute path of the malformed file.
        path: PathBuf,
        /// Deserializer message.
        message: String,
    },

    /// The file parsed as Hjson but did not match the FORMAT.md case shape.
    #[error("{path}: {field}: {message}")]
    Shape {
        /// Absolute path of the offending file.
        path: PathBuf,
        /// Dotted field path within the case (`steps[3].expect.outcome`).
        field: String,
        /// What was wrong and, where possible, how to fix it.
        message: String,
    },
}

impl LoadError {
    /// The file this error concerns.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Read { path, .. } | Self::Syntax { path, .. } | Self::Shape { path, .. } => path,
        }
    }
}

/// A cursor over one file's field path, used to build [`LoadError::Shape`]
/// errors that carry the full dotted location without threading strings by
/// hand at every call site.
#[derive(Debug, Clone)]
pub struct Loc<'a> {
    path: &'a Path,
    field: String,
}

impl<'a> Loc<'a> {
    /// Start at the root of `path`.
    #[must_use]
    pub fn root(path: &'a Path) -> Self {
        Self { path, field: String::new() }
    }

    /// The file being read.
    #[must_use]
    pub fn file(&self) -> &'a Path {
        self.path
    }

    /// Descend into a named member.
    #[must_use]
    pub fn member(&self, name: &str) -> Self {
        let field = if self.field.is_empty() {
            name.to_owned()
        } else {
            format!("{}.{name}", self.field)
        };
        Self { path: self.path, field }
    }

    /// Descend into an array index.
    #[must_use]
    pub fn index(&self, i: usize) -> Self {
        Self { path: self.path, field: format!("{}[{i}]", self.field) }
    }

    /// Build a shape error at this location.
    #[must_use]
    pub fn error(&self, message: impl Into<String>) -> LoadError {
        let field = if self.field.is_empty() { "<root>".to_owned() } else { self.field.clone() };
        LoadError::Shape { path: self.path.to_path_buf(), field, message: message.into() }
    }
}
