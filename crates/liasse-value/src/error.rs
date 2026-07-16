//! Diagnostics for value construction and wire decoding.
//!
//! Every fallible boundary in this crate returns a [`ValueError`]. Past that
//! boundary a `Value` is proof of well-formedness (AGENTS.md: parse, don't
//! validate), so the error surface is concentrated here at construction time.

use thiserror::Error;

/// The JSON shape a decoder actually found, used to phrase type-mismatch
/// diagnostics ("expected a JSON string for `int`, found a number").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonShape {
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

impl JsonShape {
    /// Classify a decoded `serde_json` value.
    #[must_use]
    pub fn of(value: &serde_json::Value) -> Self {
        match value {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(_) => Self::Bool,
            serde_json::Value::Number(_) => Self::Number,
            serde_json::Value::String(_) => Self::String,
            serde_json::Value::Array(_) => Self::Array,
            serde_json::Value::Object(_) => Self::Object,
        }
    }

    const fn describe(self) -> &'static str {
        match self {
            Self::Null => "a JSON null",
            Self::Bool => "a JSON boolean",
            Self::Number => "a JSON number",
            Self::String => "a JSON string",
            Self::Array => "a JSON array",
            Self::Object => "a JSON object",
        }
    }
}

impl core::fmt::Display for JsonShape {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.describe())
    }
}

/// Everything that can go wrong turning raw input into a canonical value.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ValueError {
    #[error("expected {expected} for `{ty}`, found {found}")]
    TypeMismatch {
        ty: &'static str,
        expected: JsonShape,
        found: JsonShape,
    },

    #[error("`int` value `{0}` is not a canonical base-10 integer")]
    MalformedInt(String),

    #[error("`decimal` value `{0}` is not a valid exact decimal")]
    MalformedDecimal(String),

    #[error(
        "`decimal` scale magnitude {magnitude} exceeds the supported bound {limit}; \
         such a value would expand to billions of digits in canonical form"
    )]
    DecimalScaleOutOfRange { magnitude: u64, limit: u64 },

    #[error("`uuid` value `{0}` is not a valid UUID")]
    MalformedUuid(String),

    #[error("`date` value `{0}` is not a valid `YYYY-MM-DD` Gregorian date")]
    MalformedDate(String),

    #[error("`timestamp` value `{0}` is not a canonical base-10 signed count")]
    MalformedTimestamp(String),

    #[error("`duration` value `{text}` is not a canonical ISO-8601 elapsed duration: {reason}")]
    MalformedDuration { text: String, reason: &'static str },

    #[error("`bytes` payload is not canonical base64: {0}")]
    MalformedBase64(String),

    #[error("`blob` `$sha512` is not 64 lowercase-hex bytes: {0}")]
    MalformedSha512(String),

    #[error(
        "fixed-period string `{0}` contains a calendar (year/month/week) component; \
         only elapsed day/time components are allowed"
    )]
    CalendarInFixedPeriod(String),

    #[error("calendar period has no non-zero magnitude component")]
    EmptyCalendarPeriod,

    #[error("unknown calendar-period policy `{value}` for `{field}`")]
    UnknownPolicy { field: &'static str, value: String },

    #[error("`enum` label `{label}` is not one of the declared labels {allowed:?}")]
    UnknownEnumLabel { label: String, allowed: Vec<String> },

    #[error("missing required member `{0}`")]
    MissingMember(String),

    #[error("unexpected member `{0}`")]
    UnexpectedMember(String),

    #[error(
        "composite ref key has {found} component(s) but the target key declares {expected}"
    )]
    CompositeArity { expected: usize, found: usize },

    #[error("type `{0}` is not eligible as a collection key component (A.8)")]
    NotKeyEligible(&'static str),

    #[error(
        "calendar-period time zone `{0}` is unavailable: no time-zone database is configured for \
         this build"
    )]
    PeriodZoneUnavailable(String),

    #[error("advancing a timestamp by this period overflows the representable calendar range")]
    PeriodOutOfRange,

    #[error("a recurrence period must advance strictly beyond the prior boundary (§14.5)")]
    NonAdvancingPeriod,

    #[error("a finite series bound must be strictly greater than its initial start (§14.5)")]
    SeriesBoundNotAfterStart,

    #[error("a recurring series exceeded the {0}-period generation bound before its horizon")]
    SeriesTooLong(usize),
}
