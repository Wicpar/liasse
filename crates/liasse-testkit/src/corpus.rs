//! The corpus loader: walk `tests/<area>/{common,red}/*.hjson`, decode each
//! file, and yield typed [`LoadedCase`]s tagged with their area and suite class.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::case::Case;
use crate::error::LoadError;
use crate::notes::ChapterNotes;

/// A chapter directory name under `tests/`, e.g. `05-state-model`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Area(String);

impl Area {
    /// Name an area directly (for a synthetic run outside the on-disk corpus).
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The directory name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Area {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Which suite a case belongs to: the normative `common` set or the adversarial
/// `red` set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuiteKind {
    /// `common/` — normative happy and ordinary error paths.
    Common,
    /// `red/` — adversarial, hostile scenarios.
    Red,
}

impl SuiteKind {
    fn from_dir(name: &str) -> Option<Self> {
        match name {
            "common" => Some(Self::Common),
            "red" => Some(Self::Red),
            _ => None,
        }
    }

    /// The directory name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Common => "common",
            Self::Red => "red",
        }
    }
}

/// A case together with its corpus location tags.
#[derive(Debug, Clone)]
pub struct LoadedCase {
    /// Absolute path of the source file.
    pub path: PathBuf,
    /// The chapter directory the case lives in.
    pub area: Area,
    /// Whether the case is a `common` or `red` case.
    pub suite_kind: SuiteKind,
    /// The parsed case.
    pub case: Case,
}

/// The whole loaded corpus.
#[derive(Debug, Clone)]
pub struct Corpus {
    /// Every case, in deterministic path order.
    pub cases: Vec<LoadedCase>,
}

impl Corpus {
    /// The corpus root baked in at build time (`<crate>/../../tests`).
    #[must_use]
    pub fn default_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests")
    }

    /// Load the corpus from its default root.
    pub fn load() -> Result<Self, LoadError> {
        Self::load_from(&Self::default_root())
    }

    /// Load every `common`/`red` case under `root`, failing on the first error.
    pub fn load_from(root: &Path) -> Result<Self, LoadError> {
        let mut cases = Vec::new();
        for result in Self::load_results_from(root)? {
            cases.push(result?);
        }
        Ok(Self { cases })
    }

    /// Attempt to load every case under `root`, returning one result per file so
    /// a caller (or the conformance test) can report every failure at once.
    /// The outer error covers only a failure to enumerate the corpus itself.
    pub fn load_results_from(root: &Path) -> Result<Vec<Result<LoadedCase, LoadError>>, LoadError> {
        let mut notes_cache: BTreeMap<String, ChapterNotes> = BTreeMap::new();

        let mut files: Vec<PathBuf> = WalkDir::new(root)
            .min_depth(3)
            .max_depth(3)
            .into_iter()
            .filter_map(Result::ok)
            .map(walkdir::DirEntry::into_path)
            .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "hjson"))
            .collect();
        files.sort();

        let mut results = Vec::with_capacity(files.len());
        for path in files {
            let Some((area_name, suite_kind)) = classify(root, &path) else {
                continue;
            };
            let notes = match notes_cache.get(&area_name) {
                Some(notes) => notes.clone(),
                None => {
                    let notes = ChapterNotes::load(&root.join(&area_name))?;
                    notes_cache.insert(area_name.clone(), notes.clone());
                    notes
                }
            };
            results.push(load_case(&path, notes.keys()).map(|case| LoadedCase {
                path,
                area: Area(area_name),
                suite_kind,
                case,
            }));
        }

        Ok(results)
    }

    /// Cases in one chapter.
    pub fn in_area<'a>(&'a self, area: &'a str) -> impl Iterator<Item = &'a LoadedCase> {
        self.cases.iter().filter(move |c| c.area.as_str() == area)
    }
}

/// Extract `(area, suite_kind)` from a `tests/<area>/<class>/<file>` path.
fn classify(root: &Path, path: &Path) -> Option<(String, SuiteKind)> {
    let relative = path.strip_prefix(root).ok()?;
    let mut components = relative.components();
    let area = components.next()?.as_os_str().to_str()?.to_owned();
    let suite = components.next()?.as_os_str().to_str()?;
    Some((area, SuiteKind::from_dir(suite)?))
}

fn load_case(path: &Path, allowed: &std::collections::BTreeSet<String>) -> Result<Case, LoadError> {
    let text = std::fs::read_to_string(path).map_err(|source| LoadError::Read { path: path.to_path_buf(), source })?;
    Case::from_hjson(&text, path, allowed)
}
