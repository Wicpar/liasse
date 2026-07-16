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
//! # Documented spec-gap choices
//!
//! Where Annex A leaves observable behavior unpinned, this crate makes the
//! least-surprising choice and cites the SPEC-ISSUES item at the definition:
//! decimal trailing-zero spelling (item 1, [`Decimal::to_canonical_text`]),
//! non-canonical input acceptance (item 2, [`Integer::parse`] / [`Uuid::parse`]),
//! and uppercase-hex SHA-512 (item 20, [`Sha512::parse`]).

mod blob;
mod decimal;
mod decode;
mod duration;
mod enumeration;
mod error;
mod int;
mod json;
mod period;
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
pub use scalars::{Bytes, Text, Uuid};
pub use temporal::{Date, Precision, Timestamp};
pub use ty::{RefTarget, StructType, Type};
pub use value::{Ref, RefKey, Struct, Value};

/// Re-exported so downstream crates share the exact big-number types the
/// canonical `int`/`decimal` values are built from.
pub use bigdecimal::{self, num_bigint};
