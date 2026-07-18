//! Client-to-server frames — the request bodies a client POSTs on one logical
//! connection (§12.1, §12.3).
//!
//! Every field a client supplies is parsed as hostile input at the server boundary
//! (AGENTS.md). The engine-interpreted payloads — `params`, `args`, `auth`,
//! `context` — are opaque [`serde_json::Value`]s here: the server decodes each
//! against the declared type it targets, so this crate never models their shape.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::token::{Occ, OperationId, Sub};

/// A frame sent from client to server. Tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Upstream {
    /// Open the connection, optionally authenticating it (§11). The server replies
    /// with a connection capability the client presents thereafter.
    Hello {
        /// A connection-level authentication selection, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth: Option<Value>,
        /// A §11.8 context selection, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<Value>,
    },
    /// Request the app's exposed manifest (§12.1). The reply carries it as a value.
    Manifest,
    /// Open (or replace) a live subscription (§12.2).
    View {
        /// The client-chosen subscription identifier echoed on downstream frames.
        sub: Sub,
        /// The exposed view address (e.g. `public.tasks.index`).
        address: String,
        /// The view's parameters, decoded server-side against their declared types.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params: Option<Value>,
        /// A bounded window over the view, if the subscription is windowed (§12.2).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<WireWindow>,
        /// A per-view authentication selection re-verified at each frontier (§11).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth: Option<Value>,
        /// A §11.8 context selection for this view.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<Value>,
    },
    /// End a live subscription.
    Unsubscribe {
        /// The subscription to end.
        sub: Sub,
    },
    /// Invoke an exposed call (§10, §12.3). Its outcome comes back as an
    /// [`crate::Outcome`]; the §12.3 operation capability travels as transport
    /// metadata, not in this body.
    Call {
        /// The exposed call address.
        address: String,
        /// The call arguments, decoded server-side against their declared types.
        args: Value,
        /// A per-request authentication selection, if any (§11).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth: Option<Value>,
        /// A §11.8 context selection for this request.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<Value>,
    },
    /// Read a value once at the current frontier (§12.1) — a snapshot read, not a
    /// subscription.
    Fetch {
        /// The exposed read address.
        address: String,
        /// The read's parameters, decoded server-side.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params: Option<Value>,
    },
    /// Query the retained status of an operation by its capability (§12.3).
    Operation {
        /// The operation whose status is requested.
        operation: OperationId,
    },
}

/// A bounded window over a row-stream view (§12.2): its size, where it anchors, and
/// whether the anchor slides to stay centered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireWindow {
    /// The maximum number of rows the window presents.
    pub size: usize,
    /// Where the window anchors in the view order.
    #[serde(default)]
    pub anchor: WireAnchor,
    /// Whether the anchor is centered in the window as far as bounds allow
    /// (`$slide: true`); no effect on a first/last window.
    #[serde(default)]
    pub slide: bool,
}

/// Where a bounded window anchors (§12.2). Tagged by `kind` so an occurrence token
/// is never confused with the `first`/`last` keywords.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireAnchor {
    /// The first rows of the view (the no-anchor default).
    #[default]
    First,
    /// The last rows of the view.
    Last,
    /// A window anchored on a specific occurrence (§12.2 concrete anchor).
    At {
        /// The occurrence the window anchors on.
        occ: Occ,
    },
}
