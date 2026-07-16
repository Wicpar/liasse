//! The typed outcome of a call (§9.4, §22.7). Application rejections are an
//! outcome, not an error — only store/engine failures are [`EngineError`].

use liasse_store::CommitSeq;

use crate::error::Rejection;
use crate::response::ResponseValue;

/// The result of admitting a mutation call.
#[derive(Debug, Clone)]
pub enum CallOutcome {
    /// The request changed state and took serial position `seq`; `response` is
    /// the `return` evaluated from committed state (§8.6, §8.10).
    Committed { seq: CommitSeq, response: Option<ResponseValue> },
    /// The request produced no state change (§8.9): no commit, no advanced
    /// frontier; any `return` was evaluated from the unchanged state.
    Unchanged { response: Option<ResponseValue> },
    /// The rule pipeline refused the request; committed state is intact.
    Rejected(Rejection),
}

impl CallOutcome {
    /// The rejection, if the request was refused.
    #[must_use]
    pub fn rejection(&self) -> Option<&Rejection> {
        match self {
            Self::Rejected(rejection) => Some(rejection),
            _ => None,
        }
    }

    /// The serial position, if the request committed.
    #[must_use]
    pub fn committed_at(&self) -> Option<CommitSeq> {
        match self {
            Self::Committed { seq, .. } => Some(*seq),
            _ => None,
        }
    }

    /// The response value delivered with the outcome, if any.
    #[must_use]
    pub fn response(&self) -> Option<&ResponseValue> {
        match self {
            Self::Committed { response, .. } | Self::Unchanged { response } => response.as_ref(),
            Self::Rejected(_) => None,
        }
    }
}
