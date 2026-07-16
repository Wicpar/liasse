//! `duration` — exact elapsed duration (A.1), also the fixed-period form (A.4).

use crate::error::ValueError;

const NS_PER_SEC: i128 = 1_000_000_000;
const NS_PER_MIN: i128 = 60 * NS_PER_SEC;
const NS_PER_HOUR: i128 = 60 * NS_PER_MIN;
const NS_PER_DAY: i128 = 24 * NS_PER_HOUR;

/// An exact elapsed duration held as a signed nanosecond count.
///
/// "Elapsed" means calendar-free: only day and time components (A.4), so the
/// value is a plain quantity of time. Ordering (B.1) is the natural order of
/// that count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Duration {
    nanos: i128,
}

impl Duration {
    /// The zero duration, canonical text `PT0S`.
    pub const ZERO: Self = Self { nanos: 0 };

    /// Build from a signed nanosecond count.
    #[must_use]
    pub const fn from_nanos(nanos: i128) -> Self {
        Self { nanos }
    }

    /// The signed nanosecond count.
    #[must_use]
    pub const fn as_nanos(self) -> i128 {
        self.nanos
    }

    /// Whether every magnitude component is zero.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.nanos == 0
    }

    /// Parse a canonical ISO-8601 elapsed duration string.
    ///
    /// Only day/time components are accepted (A.4). A year, month, or week
    /// component makes the string a *calendar* quantity, rejected here with
    /// [`ValueError::CalendarInFixedPeriod`] so a calendar recurrence cannot be
    /// smuggled through the fixed-period string form.
    pub fn parse(text: &str) -> Result<Self, ValueError> {
        Scanner::new(text).run()
    }

    /// The canonical ISO-8601 spelling (A.1). Largest-to-smallest day/time
    /// components, zero components omitted, fractional seconds trimmed; the
    /// zero duration is `PT0S`.
    #[must_use]
    pub fn to_canonical_text(&self) -> String {
        if self.nanos == 0 {
            return "PT0S".to_owned();
        }
        let sign = if self.nanos < 0 { "-" } else { "" };
        let mut n = self.nanos.unsigned_abs();
        let day_u = NS_PER_DAY.unsigned_abs();
        let hour_u = NS_PER_HOUR.unsigned_abs();
        let min_u = NS_PER_MIN.unsigned_abs();
        let sec_u = NS_PER_SEC.unsigned_abs();
        let days = n / day_u;
        n %= day_u;
        let hours = n / hour_u;
        n %= hour_u;
        let minutes = n / min_u;
        n %= min_u;
        let seconds = n / sec_u;
        let frac = n % sec_u;

        let mut out = String::from("P");
        if days > 0 {
            out.push_str(&format!("{days}D"));
        }
        if hours > 0 || minutes > 0 || seconds > 0 || frac > 0 {
            out.push('T');
            if hours > 0 {
                out.push_str(&format!("{hours}H"));
            }
            if minutes > 0 {
                out.push_str(&format!("{minutes}M"));
            }
            if seconds > 0 || frac > 0 {
                if frac == 0 {
                    out.push_str(&format!("{seconds}S"));
                } else {
                    let frac_str = format!("{frac:09}");
                    let trimmed = frac_str.trim_end_matches('0');
                    out.push_str(&format!("{seconds}.{trimmed}S"));
                }
            }
        }
        format!("{sign}{out}")
    }
}

/// A single-pass ISO-8601 duration reader over the elapsed subset.
struct Scanner<'a> {
    text: &'a str,
    rest: &'a str,
    negative: bool,
    nanos: i128,
    saw_component: bool,
    in_time: bool,
}

impl<'a> Scanner<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            rest: text,
            negative: false,
            nanos: 0,
            saw_component: false,
            in_time: false,
        }
    }

    fn malformed(&self, reason: &'static str) -> ValueError {
        ValueError::MalformedDuration {
            text: self.text.to_owned(),
            reason,
        }
    }

    fn run(mut self) -> Result<Duration, ValueError> {
        if let Some(stripped) = self.rest.strip_prefix('-') {
            self.negative = true;
            self.rest = stripped;
        } else if let Some(stripped) = self.rest.strip_prefix('+') {
            self.rest = stripped;
        }
        self.rest = self
            .rest
            .strip_prefix('P')
            .ok_or_else(|| self.malformed("missing `P` designator"))?;

        while !self.rest.is_empty() {
            if let Some(stripped) = self.rest.strip_prefix('T') {
                if self.in_time {
                    return Err(self.malformed("duplicate `T` time separator"));
                }
                self.in_time = true;
                self.rest = stripped;
                continue;
            }
            self.consume_component()?;
        }

        if !self.saw_component {
            return Err(self.malformed("duration has no components"));
        }
        let signed = if self.negative { -self.nanos } else { self.nanos };
        Ok(Duration::from_nanos(signed))
    }

    fn consume_component(&mut self) -> Result<(), ValueError> {
        let digit_end = self
            .rest
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .ok_or_else(|| self.malformed("number without a unit designator"))?;
        let (number, tail) = self.rest.split_at(digit_end);
        let unit = tail
            .chars()
            .next()
            .ok_or_else(|| self.malformed("number without a unit designator"))?;
        self.rest = tail
            .get(unit.len_utf8()..)
            .ok_or_else(|| self.malformed("truncated component"))?;

        let per_unit = self.unit_scale(unit)?;
        if unit == 'S' {
            self.add_seconds(number, per_unit)?;
        } else {
            if number.contains('.') {
                return Err(self.malformed("only the seconds component may be fractional"));
            }
            let magnitude: i128 = number
                .parse()
                .map_err(|_| self.malformed("component magnitude is not an integer"))?;
            self.nanos = self
                .nanos
                .checked_add(magnitude.checked_mul(per_unit).ok_or_else(|| {
                    self.malformed("duration magnitude overflows the representable range")
                })?)
                .ok_or_else(|| self.malformed("duration overflows the representable range"))?;
        }
        self.saw_component = true;
        Ok(())
    }

    fn unit_scale(&self, unit: char) -> Result<i128, ValueError> {
        match (self.in_time, unit) {
            (false, 'D') => Ok(NS_PER_DAY),
            (false, 'Y' | 'M' | 'W') => Err(ValueError::CalendarInFixedPeriod(self.text.to_owned())),
            (true, 'H') => Ok(NS_PER_HOUR),
            (true, 'M') => Ok(NS_PER_MIN),
            (true, 'S') => Ok(NS_PER_SEC),
            _ => Err(self.malformed("unexpected unit designator")),
        }
    }

    fn add_seconds(&mut self, number: &str, per_unit: i128) -> Result<(), ValueError> {
        let (whole, frac) = number.split_once('.').unwrap_or((number, ""));
        if frac.len() > 9 {
            return Err(self.malformed("fractional seconds finer than nanoseconds"));
        }
        let whole_val: i128 = whole
            .parse()
            .map_err(|_| self.malformed("seconds value is not an integer"))?;
        let mut frac_nanos: i128 = 0;
        if !frac.is_empty() {
            let padded = format!("{frac:0<9}");
            frac_nanos = padded
                .parse()
                .map_err(|_| self.malformed("fractional seconds are not numeric"))?;
        }
        let secs_nanos = whole_val
            .checked_mul(per_unit)
            .ok_or_else(|| self.malformed("seconds overflow the representable range"))?;
        self.nanos = self
            .nanos
            .checked_add(secs_nanos)
            .and_then(|v| v.checked_add(frac_nanos))
            .ok_or_else(|| self.malformed("duration overflows the representable range"))?;
        Ok(())
    }
}
