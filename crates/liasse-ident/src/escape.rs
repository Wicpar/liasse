//! The D.2 / D.3 percent codec.
//!
//! Annex D reserves three characters inside canonical identity text: `%`, `/`,
//! and `:`. Each original occurrence is encoded as `%25`, `%2F`, and `%3A`
//! respectively; the percent signs introduced by these escapes are never
//! encoded again (D.2). A single left-to-right pass realizes exactly that,
//! because emitted escapes are not re-scanned.

use crate::error::IdentError;

/// Which reserved characters a context escapes. Key components escape all three
/// reserved characters; declaration-name path segments escape only `%` and `/`
/// (D.3 mentions no `:` for name segments, and application names cannot contain
/// one), so `:` is a literal there.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Codec {
    escape_colon: bool,
}

impl Codec {
    /// The scalar-key-component codec (D.2): escapes `%`, `/`, and `:`.
    pub(crate) const KEY: Codec = Codec { escape_colon: true };

    /// The declaration-name segment codec (D.3): escapes `%` and `/` only.
    pub(crate) const NAME: Codec = Codec { escape_colon: false };

    /// Encode raw text into its canonical escaped form.
    pub(crate) fn encode(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for c in text.chars() {
            match c {
                '%' => out.push_str("%25"),
                '/' => out.push_str("%2F"),
                ':' if self.escape_colon => out.push_str("%3A"),
                other => out.push(other),
            }
        }
        out
    }

    /// Decode canonical escaped text back to its raw form. Rejects any escape
    /// outside this codec's alphabet — a `%2f`, a bare `%`, or (for a name
    /// segment) a `%3A` are not canonical output of this codec and so cannot
    /// appear in conformant input.
    pub(crate) fn decode(&self, text: &str) -> Result<String, IdentError> {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            match (chars.next(), chars.next()) {
                (Some('2'), Some('5')) => out.push('%'),
                (Some('2'), Some('F')) => out.push('/'),
                (Some('3'), Some('A')) if self.escape_colon => out.push(':'),
                _ => {
                    return Err(IdentError::MalformedEscape {
                        text: text.to_owned(),
                    });
                }
            }
        }
        Ok(out)
    }

    /// Confirm every escape in `text` is well-formed without materializing the
    /// decoded string. Used to validate a still-encoded path segment whose
    /// name-or-key role is not yet known, so the permissive [`Codec::KEY`]
    /// alphabet is the right check.
    pub(crate) fn validate(&self, text: &str) -> Result<(), IdentError> {
        self.decode(text).map(|_| ())
    }
}
