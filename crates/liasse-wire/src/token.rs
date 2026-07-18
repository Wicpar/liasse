//! Opaque capability tokens carried on the wire.
//!
//! The connector never puts an internal identity on the wire (AGENTS.md, SPEC.md
//! §12): a [`RowId`](../../liasse-runtime) canonical key, a raw `CommitSeq`, or a
//! host session id would leak model structure and grant unintended reach. Each is
//! projected to an opaque, high-entropy string capability that is meaningful only
//! to the peer that minted it. These newtypes make that projection a distinct type
//! rather than a bare `String`: an occurrence token cannot be passed where a
//! frontier token is expected, and neither can be confused with a connection or
//! operation capability.
//!
//! S1 models the tokens as opaque strings; minting, per-connection nonces, and
//! forgery checks live server-side in `liasse-connect` (the tokens are inert data
//! to a client, which only echoes them back).

/// Generate an opaque string-newtype capability token with a uniform surface.
///
/// Each token is `#[serde(transparent)]`, so on the wire it is the bare string —
/// there is no envelope to model per token, and the JSON is exactly the capability
/// text. The type is `Ord`/`Hash` so a store can key retained state by it.
macro_rules! opaque_token {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash,
            serde::Serialize, serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wrap an already-minted capability string.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// The capability text as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consume the token, yielding its capability string.
            #[must_use]
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    };
}

opaque_token! {
    /// A per-subscription occurrence token (§12.2 `$id`): the opaque projection of a
    /// row's internal `RowId`. The bijection between a `RowId` and its `Occ` is
    /// per-subscription and dies with it, so the same token never crosses
    /// subscriptions and reveals nothing about the keyed identity behind it.
    Occ
}

opaque_token! {
    /// A frontier token (§12.2): the opaque projection of a connection `CommitSeq`,
    /// bound to the connection epoch so it is meaningful only within that
    /// connection. It is what an SSE `id:` carries, giving `Last-Event-ID` resume.
    Ft
}

opaque_token! {
    /// A subscription identifier chosen by the client and echoed on every
    /// per-subscription downstream frame so the client can route it to the right
    /// live view.
    Sub
}

opaque_token! {
    /// A connection capability minted by the server in response to `hello`. It names
    /// the one logical connection that is the §12.3 coherence unit; a client
    /// presents it on every subsequent request.
    ConnectionToken
}

opaque_token! {
    /// A per-client operation capability (§12.3): the idempotency identifier that
    /// makes a submission at-most-once. Presenting the same one replays the retained
    /// outcome; a different request under it is rejected.
    OperationId
}
