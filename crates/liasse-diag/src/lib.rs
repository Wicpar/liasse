//! Diagnostics: source spans, severities, labeled annotations, and hints,
//! rendered rustc-style. Every user-facing error in the workspace is a
//! diagnostic from this crate; a diagnostic must let a new user understand
//! what went wrong, why, and (when a hint applies) how to fix it.
