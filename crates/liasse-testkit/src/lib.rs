//! Loader and typed model for the file-based conformance corpus under `tests/`
//! (see `tests/FORMAT.md`), plus the harness-facing execution contract.
//!
//! This crate makes the corpus machine-checkable before the runtime exists. It
//! has three layers:
//!
//! - **Case model** ([`Case`], [`Step`], [`Expect`], [`Outcome`], [`Matcher`]):
//!   parse-don't-validate types for the full FORMAT.md vocabulary. Embedded
//!   package and host definitions stay as raw JSON — the language parser owns
//!   their meaning later.
//! - **Corpus loader** ([`Corpus`]): walks `tests/<area>/{common,red}/*.hjson`,
//!   tags each case with its area and suite class, and validates every step key
//!   against the chapter's `NOTES.md` ([`ChapterNotes`]).
//! - **Execution contract** ([`Executor`], [`Observation`], [`Verdict`],
//!   [`Report`]): the trait a future runtime adapter implements, decoupled from
//!   the loader so an executor can be written without touching it.
//!
//! ```no_run
//! let corpus = liasse_testkit::Corpus::load()?;
//! for loaded in &corpus.cases {
//!     println!("{}/{} :: {}", loaded.area, loaded.suite_kind.as_str(), loaded.case.name);
//! }
//! # Ok::<(), liasse_testkit::LoadError>(())
//! ```

mod anchor;
mod case;
mod contract;
mod corpus;
mod error;
mod expect;
mod id;
mod matcher;
mod notes;
mod outcome;
mod relax;
mod report;
mod step;
mod step_kind;

pub use anchor::{AnchorKind, SpecAnchor};
pub use case::{Case, CaseBody, PackageSet, Suite};
pub use contract::{CallRequest, ConnectRequest, Executor, Observation, WatchRequest};
pub use corpus::{Area, Corpus, LoadedCase, SuiteKind};
pub use error::{Loc, LoadError};
pub use expect::Expect;
pub use id::{ArtifactLabel, BindName, ConnectionId, WatchId};
pub use matcher::{Bindings, MatchError, Matcher};
pub use notes::ChapterNotes;
pub use outcome::{Completion, OperationStatus, Outcome};
pub use report::{check_expectation, CaseResult, Report, Verdict};
pub use step::{Nested, Step};
pub use step_kind::{StepKind, StepScope};
