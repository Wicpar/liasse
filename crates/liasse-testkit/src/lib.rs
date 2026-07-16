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
//! - **Execution contract** ([`Driver`], [`Observation`], [`Request`]): the
//!   trait a future runtime adapter implements, decoupled from the loader so an
//!   executor can be written without touching it.
//! - **Scenario executor** ([`Engine`], [`run_case`], [`StepTrace`],
//!   [`CaseVerdict`], [`ConformanceSummary`]): interprets a loaded scenario case
//!   step by step against a [`Driver`], judging every expectation with the
//!   matcher language and producing a per-case verdict. [`fake::FakeDriver`] is
//!   the scripted double the engine's own tests drive.
//!
//! ```no_run
//! let corpus = liasse_testkit::Corpus::load()?;
//! for loaded in &corpus.cases {
//!     println!("{}/{} :: {}", loaded.area, loaded.suite_kind.as_str(), loaded.case.name);
//! }
//! # Ok::<(), liasse_testkit::LoadError>(())
//! ```

pub mod adapter;
mod anchor;
mod case;
mod clock;
mod contract;
mod corpus;
mod engine;
mod error;
mod expect;
pub mod fake;
mod hosts;
mod id;
mod matcher;
mod notes;
mod outcome;
mod relax;
mod report;
pub mod scenario_gate;
mod request;
mod step;
mod step_kind;
mod trace;
mod view;

pub use adapter::{AdapterError, MemoryProvision, ScenarioAdapter, StoreProvision};
pub use anchor::{AnchorKind, SpecAnchor};
pub use case::{Case, CaseBody, PackageSet, Suite};
pub use clock::{DurationParseError, Instant, Iso8601Duration, VirtualClock};
pub use contract::{CallRequest, ConnectRequest, Driver, Observation, WatchRequest, is_structural};
pub use corpus::{Area, Corpus, LoadedCase, SuiteKind};
pub use engine::{run_case, run_loaded, Engine};
pub use error::{Loc, LoadError};
pub use expect::Expect;
pub use hosts::{HostComponent, HostKind, HostsConfig};
pub use id::{ArtifactLabel, BindName, ConnectionId, WatchId};
pub use matcher::{Bindings, MatchError, Matcher};
pub use notes::ChapterNotes;
pub use outcome::{Completion, OperationStatus, Outcome};
pub use report::{
    check_expectation, AreaTally, CaseResult, CaseVerdict, ConformanceSummary, Report, Verdict,
};
pub use request::{LowerError, OpRequest, Request};
pub use step::{Nested, Step};
pub use step_kind::{StepKind, StepScope};
pub use trace::{StepResult, StepTrace};
pub use view::ViewAssertion;
