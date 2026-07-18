//! Values: the canonical type system and runtime values of SPEC.md Annex A
//! (types and canonical wire values) and Annex B (deterministic total order).
//!
//! # What this crate owns
//!
//! - [`Type`] — the Liasse type model (Annex A / A.2).
//! - [`Value`] — a runtime value, well-formed by construction. Each variant is
//!   backed by a semantic newtype ([`Integer`], [`Decimal`], [`Timestamp`], …)
//!   so a bare scalar can never masquerade as a typed value.
//! - Canonical wire encoding ([`Value::to_wire`], [`Value::to_canonical_json_string`])
//!   and decoding ([`Type::decode`]) per Annex A.
//! - The deterministic total order (Annex B) as [`Ord`] on [`Value`].
//!
//! # Parse, don't validate
//!
//! Values are only ever built through fallible constructors that parse raw
//! input into the richer type. Past that boundary a [`Value`] is proof the
//! data conforms; nothing downstream re-checks it.
//!
//! # Two decode boundaries
//!
//! Annex A.1 / D.2 pin one canonical wire spelling per scalar. [`Type::decode`]
//! is the lenient **human-authoring boundary** (Annex C `$data`/`$default`) and
//! internal round-trip codec: a non-canonical scalar is accepted and
//! canonicalized. [`Type::decode_wire`] is the strict **machine wire/request
//! boundary**: a non-canonical scalar is rejected as malformed (SPEC-ISSUES
//! item 2). The scalar constructors ([`Integer::parse`], [`Uuid::parse`],
//! [`Sha512::parse`], …) implement the lenient/normalizing parse that authoring
//! relies on; [`Type::decode_wire`] layers the canonicality gate on top.
//!
//! # Documented spec-gap choices
//!
//! Where Annex A still leaves observable behavior unpinned, this crate makes the
//! least-surprising choice and cites the SPEC-ISSUES item at the definition:
//! decimal trailing-zero spelling (item 1, [`Decimal::to_canonical_text`]).
//!
//! The blob descriptor's `$sha512`/`$bytes` members are now pinned (item 20,
//! resolved with item 2): [`Sha512::parse`] is the lenient authoring parse, and
//! [`Type::decode_wire`] rejects a non-canonical descriptor member (uppercase
//! hex, a leading-zero byte count) exactly as it rejects every other
//! non-canonical scalar at the wire boundary.

mod blob;
mod decimal;
mod decode;
mod duration;
mod enumeration;
mod error;
mod int;
mod json;
mod period;
mod recur;
mod scalars;
mod temporal;
mod ty;
mod value;

pub use blob::{BlobDescriptor, MediaType, Sha512};
pub use decimal::Decimal;
pub use duration::Duration;
pub use enumeration::{EnumType, EnumValue};
pub use error::{JsonShape, ValueError};
pub use int::Integer;
pub use json::Json;
pub use period::{
    Ambiguous, CalendarPeriod, CalendarPeriodBuilder, Missing, Overflow, Period,
};
pub use recur::{recurring_intervals, Interval};
pub use scalars::{Bytes, Text, Uuid};
pub use temporal::{Date, Precision, Timestamp};
pub use ty::{RefTarget, StructType, Type};
pub use value::{Ref, RefKey, Struct, Value};

/// Re-exported so downstream crates share the exact big-number types the
/// canonical `int`/`decimal` values are built from.
pub use bigdecimal::{self, num_bigint};
