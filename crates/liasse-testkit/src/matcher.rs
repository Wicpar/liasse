//! The matcher tree and its binding environment.
//!
//! FORMAT.md's "Determinism and matchers" section defines how expected values
//! are written so cases never depend on real randomness or wall-clock time.
//! [`Matcher::parse`] turns an expected JSON value into a typed tree; a future
//! executor drives [`Matcher::check`] against an observed value, threading a
//! [`Bindings`] so `$bind:NAME` captures a generated value and `$ref:NAME`
//! reuses it.

use std::fmt;

use serde_json::Value;

/// The special member key that opens an object to extra members (`"...": true`).
pub const OPEN_MARKER: &str = "...";
/// The member key that introduces an order-insensitive array match.
pub const UNORDERED_MARKER: &str = "$unordered";

/// A typed expected value.
#[derive(Debug, Clone, PartialEq)]
pub enum Matcher {
    /// `"$any"` — matches any value.
    Any,
    /// `"$any:uuid"` — any well-formed uuid string.
    AnyUuid,
    /// `"$any:timestamp"` — any well-formed timestamp string.
    AnyTimestamp,
    /// `"$bind:NAME"` — matches any value and binds it as `NAME`.
    Bind(String),
    /// `"$ref:NAME"` — matches exactly the value previously bound to `NAME`.
    Ref(String),
    /// `"$absent"` — as an object member value, requires the member to be absent.
    Absent,
    /// `{ $unordered: [..] }` — the array ignoring order.
    Unordered(Vec<Matcher>),
    /// An ordered array; every element matched positionally.
    Array(Vec<Matcher>),
    /// An object. `open` is set by the `"...": true` extra-members marker.
    Object {
        /// Expected members, in authored order.
        members: Vec<(String, Matcher)>,
        /// Whether extra observed members are permitted.
        open: bool,
    },
    /// A literal scalar matched by exact JSON equality.
    Scalar(Value),
}

impl Matcher {
    /// Parse an expected JSON value into a matcher tree.
    ///
    /// This never fails: any JSON value is a valid expectation (an unrecognized
    /// `$`-string is treated as a literal string to match exactly).
    #[must_use]
    pub fn parse(value: &Value) -> Self {
        match value {
            Value::String(s) => Self::parse_string(s),
            Value::Array(items) => Self::Array(items.iter().map(Self::parse).collect()),
            Value::Object(map) => Self::parse_object(map),
            other => Self::Scalar(other.clone()),
        }
    }

    fn parse_string(s: &str) -> Self {
        match s {
            "$any" => Self::Any,
            "$any:uuid" => Self::AnyUuid,
            "$any:timestamp" => Self::AnyTimestamp,
            "$absent" => Self::Absent,
            _ => {
                if let Some(name) = s.strip_prefix("$bind:") {
                    Self::Bind(name.to_owned())
                } else if let Some(name) = s.strip_prefix("$ref:") {
                    Self::Ref(name.to_owned())
                } else {
                    Self::Scalar(Value::String(s.to_owned()))
                }
            }
        }
    }

    fn parse_object(map: &serde_json::Map<String, Value>) -> Self {
        // A single `$unordered` array is the set matcher; otherwise an object.
        if map.len() == 1
            && let Some(Value::Array(items)) = map.get(UNORDERED_MARKER)
        {
            return Self::Unordered(items.iter().map(Self::parse).collect());
        }
        let mut open = false;
        let mut members = Vec::with_capacity(map.len());
        for (key, val) in map {
            if key == OPEN_MARKER {
                open = matches!(val, Value::Bool(true));
                continue;
            }
            members.push((key.clone(), Self::parse(val)));
        }
        Self::Object { members, open }
    }

    /// Check an observed value against this matcher, updating `env` with any
    /// `$bind:` captures. Returns the first mismatch found.
    pub fn check(&self, observed: &Value, env: &mut Bindings) -> Result<(), MatchError> {
        self.check_at(observed, env, &mut String::from("$"))
    }

    fn check_at(&self, observed: &Value, env: &mut Bindings, path: &mut String) -> Result<(), MatchError> {
        match self {
            Self::Any => Ok(()),
            Self::AnyUuid => require(is_uuid(observed), path, "expected a uuid"),
            Self::AnyTimestamp => require(is_timestamp(observed), path, "expected a timestamp"),
            Self::Bind(name) => {
                env.bind(name.clone(), observed.clone());
                Ok(())
            }
            Self::Ref(name) => match env.get(name) {
                Some(bound) if bound == observed => Ok(()),
                Some(_) => Err(MatchError::at(path, format!("value differs from bound `{name}`"))),
                None => Err(MatchError::at(path, format!("`$ref:{name}` is not bound"))),
            },
            Self::Absent => Err(MatchError::at(path, "expected member to be absent")),
            Self::Scalar(expected) => require(scalar_equal(expected, observed), path, "literal value mismatch"),
            Self::Array(items) => Self::check_array(items, observed, env, path),
            Self::Unordered(items) => Self::check_unordered(items, observed, env, path),
            Self::Object { members, open } => Self::check_object(members, *open, observed, env, path),
        }
    }

    fn check_array(items: &[Matcher], observed: &Value, env: &mut Bindings, path: &mut String) -> Result<(), MatchError> {
        let Value::Array(actual) = observed else {
            return Err(MatchError::at(path, "expected an array"));
        };
        if actual.len() != items.len() {
            return Err(MatchError::at(path, format!("expected {} elements, found {}", items.len(), actual.len())));
        }
        for (i, (m, v)) in items.iter().zip(actual).enumerate() {
            let mark = path.len();
            path.push_str(&format!("[{i}]"));
            m.check_at(v, env, path)?;
            path.truncate(mark);
        }
        Ok(())
    }

    fn check_object(members: &[(String, Matcher)], open: bool, observed: &Value, env: &mut Bindings, path: &mut String) -> Result<(), MatchError> {
        let Value::Object(actual) = observed else {
            return Err(MatchError::at(path, "expected an object"));
        };
        let mut matched = 0usize;
        for (key, m) in members {
            let mark = path.len();
            path.push('.');
            path.push_str(key);
            match (m, actual.get(key)) {
                (Matcher::Absent, None) => {}
                (Matcher::Absent, Some(_)) => return Err(MatchError::at(path, "expected member to be absent")),
                (_, None) => return Err(MatchError::at(path, "expected member is missing")),
                (_, Some(v)) => {
                    matched += 1;
                    m.check_at(v, env, path)?;
                }
            }
            path.truncate(mark);
        }
        if !open && actual.len() != matched {
            return Err(MatchError::at(path, "object has unexpected extra members (add `\"...\": true` to allow)"));
        }
        Ok(())
    }

    fn check_unordered(items: &[Matcher], observed: &Value, env: &mut Bindings, path: &str) -> Result<(), MatchError> {
        let Value::Array(actual) = observed else {
            return Err(MatchError::at(path, "expected an array (unordered)"));
        };
        if actual.len() != items.len() {
            return Err(MatchError::at(path, format!("expected {} elements, found {}", items.len(), actual.len())));
        }
        let mut used = vec![false; actual.len()];
        match assign_unordered(items, actual, &mut used, env) {
            true => Ok(()),
            false => Err(MatchError::at(path, "no order-independent assignment matches every element")),
        }
    }
}

/// Backtracking bijection between matchers and array elements. On the accepting
/// path the successful `$bind:` captures remain in `env`.
fn assign_unordered(items: &[Matcher], actual: &[Value], used: &mut [bool], env: &mut Bindings) -> bool {
    let Some((first, rest)) = items.split_first() else {
        return true;
    };
    for (i, candidate) in actual.iter().enumerate() {
        if used.get(i).copied().unwrap_or(true) {
            continue;
        }
        let mut trial = env.clone();
        if first.check(candidate, &mut trial).is_ok() {
            if let Some(slot) = used.get_mut(i) {
                *slot = true;
            }
            if assign_unordered(rest, actual, used, &mut trial) {
                *env = trial;
                return true;
            }
            if let Some(slot) = used.get_mut(i) {
                *slot = false;
            }
        }
    }
    false
}

fn require(ok: bool, path: &str, message: &str) -> Result<(), MatchError> {
    if ok { Ok(()) } else { Err(MatchError::at(path, message)) }
}

/// Whether an expected literal scalar matches an observed one. Exact JSON
/// equality, widened by one canonicalization: an `int` renders on the wire as a
/// JSON string of canonical base-10 digits (Annex A.1), but the Annex-B ordering
/// corpus authors `int` expectations as bare Hjson numbers (`-10`, per that
/// chapter's `NOTES.md` "Scalar wire forms"). An expected integer *number* and an
/// observed *string* therefore denote the same `int` value when the string is the
/// number's canonical base-10 spelling. Both directions are covered so a case may
/// author either wire form. No other scalar coercion is performed.
fn scalar_equal(expected: &Value, observed: &Value) -> bool {
    if expected == observed {
        return true;
    }
    match (expected, observed) {
        (Value::Number(number), Value::String(text)) | (Value::String(text), Value::Number(number)) => {
            is_canonical_int_string(text) && number.as_i128().is_some_and(|n| n.to_string() == *text)
        }
        _ => false,
    }
}

/// Whether `text` is the canonical base-10 spelling of an integer: an optional
/// leading `-` then digits, with no leading zeros beyond a lone `"0"` and no
/// `"-0"` (Annex A.1 canonical `int` form).
fn is_canonical_int_string(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let no_leading_zero = digits == "0" || !digits.starts_with('0');
    let not_negative_zero = !(text.starts_with('-') && digits == "0");
    no_leading_zero && not_negative_zero
}

fn is_uuid(value: &Value) -> bool {
    let Value::String(s) = value else { return false };
    let groups = [8usize, 4, 4, 4, 12];
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == groups.len()
        && parts.iter().zip(groups).all(|(part, len)| part.len() == len && part.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn is_timestamp(value: &Value) -> bool {
    let Value::String(s) = value else { return false };
    if s.is_empty() {
        return false;
    }
    // Canonical wire form is a base-10 microsecond string (Annex A.1/A.5); an
    // ISO-8601 seed form carries a `T`. Accept either shape.
    let digits = s.strip_prefix('-').unwrap_or(s);
    digits.bytes().all(|b| b.is_ascii_digit()) || s.contains('T')
}

/// The values bound by `$bind:` matchers, keyed by bind name.
#[derive(Debug, Clone, Default)]
pub struct Bindings {
    map: std::collections::BTreeMap<String, Value>,
}

impl Bindings {
    /// An empty environment.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind (or rebind) `name` to `value`.
    pub fn bind(&mut self, name: String, value: Value) {
        self.map.insert(name, value);
    }

    /// The value bound to `name`, if any.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.map.get(name)
    }

    /// Replace every `"$ref:NAME"` string in `value` with its bound value,
    /// recursing through arrays and objects. Used to build outgoing `args`.
    #[must_use]
    pub fn resolve(&self, value: &Value) -> Value {
        match value {
            Value::String(s) => match s.strip_prefix("$ref:").and_then(|n| self.map.get(n)) {
                Some(bound) => bound.clone(),
                None => value.clone(),
            },
            Value::Array(items) => Value::Array(items.iter().map(|v| self.resolve(v)).collect()),
            Value::Object(map) => {
                Value::Object(map.iter().map(|(k, v)| (k.clone(), self.resolve(v))).collect())
            }
            other => other.clone(),
        }
    }
}

/// A mismatch between an observed value and a matcher, located by JSON path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchError {
    /// JSON-path-like location of the mismatch (`$.rows[0].id`).
    pub path: String,
    /// Human-readable reason.
    pub reason: String,
}

impl MatchError {
    fn at(path: &str, reason: impl Into<String>) -> Self {
        Self { path: path.to_owned(), reason: reason.into() }
    }
}

impl fmt::Display for MatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.reason)
    }
}

impl std::error::Error for MatchError {}
