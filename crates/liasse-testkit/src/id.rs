//! Semantic identifier newtypes used throughout the case model.
//!
//! These wrap the opaque string handles the corpus uses to name logical
//! objects (connections, subscriptions, artifact labels, bound values). They
//! carry no grammar of their own — the corpus author picks the spelling — but
//! keeping them distinct types stops a connection id being passed where a watch
//! id is meant.

use std::fmt;

macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// Wrap a raw handle.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// The underlying handle text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
    };
}

string_id! {
    /// A logical client connection opened by `connect` and referenced by `on`.
    ConnectionId
}
string_id! {
    /// A live subscription opened by `watch`/`resume` and referenced by `id`.
    WatchId
}
string_id! {
    /// A case-global label naming an artifact produced by `export`/`build_artifact`.
    ArtifactLabel
}
string_id! {
    /// A name introduced by a `$bind:NAME` matcher and reused via `$ref:NAME`.
    BindName
}
