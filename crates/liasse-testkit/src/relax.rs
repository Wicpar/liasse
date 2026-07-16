//! Bareword normalization for the corpus's Hjson dialect.
//!
//! The corpus is authored in Hjson but relies on a relaxation the three
//! reference Hjson parsers (deser-hjson, hjson-js, hjson-py) do not implement:
//! an unquoted token used as a *value* terminates at structural punctuation
//! (`,`, `}`, `]`) or whitespace, not greedily at end of line. FORMAT.md's own
//! "bare outcome tokens" convention (`{ outcome: ok }` written inline) depends
//! on it, and ~62% of the corpus uses it, so it is the corpus's binding dialect
//! rather than an authoring slip.
//!
//! A quoteless single token *is* a string in Hjson, so quoting it changes no
//! meaning: this pass rewrites each value-position bareword (other than
//! `true`/`false`/`null`) to a quoted string, leaving keys, numbers, strings,
//! multiline `'''` blocks, and every comment form untouched. deser-hjson then
//! parses the result, since the only construct it mishandles is the greedy
//! quoteless value.

/// Where the next token sits: an object key, a value/element, or neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    Key,
    Value,
    Other,
}

/// A single left-to-right pass that quotes value-position barewords.
struct Relaxer {
    chars: Vec<char>,
    pos: usize,
    out: String,
    /// Container stack: `true` for an object, `false` for an array.
    stack: Vec<bool>,
    expect: Expect,
}

impl Relaxer {
    fn new(input: &str) -> Self {
        Self { chars: input.chars().collect(), pos: 0, out: String::with_capacity(input.len() + 16), stack: Vec::new(), expect: Expect::Value }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.pos + ahead).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek();
        if ch.is_some() {
            self.pos += 1;
        }
        ch
    }

    fn push(&mut self, ch: char) {
        self.out.push(ch);
    }

    fn run(mut self) -> String {
        while let Some(ch) = self.peek() {
            match ch {
                '"' => self.copy_double_string(),
                '\'' => self.copy_quote(),
                '/' if self.peek_at(1) == Some('/') => self.copy_line_comment(),
                '#' => self.copy_line_comment(),
                '/' if self.peek_at(1) == Some('*') => self.copy_block_comment(),
                '{' => self.open('{', true, Expect::Key),
                '[' => self.open('[', false, Expect::Value),
                '}' | ']' => self.close(),
                ':' => {
                    self.bump();
                    self.push(':');
                    self.expect = Expect::Value;
                }
                ',' => {
                    self.bump();
                    self.push(',');
                    self.expect = match self.stack.last() {
                        Some(true) => Expect::Key,
                        Some(false) => Expect::Value,
                        None => Expect::Other,
                    };
                }
                c if c.is_whitespace() => {
                    self.bump();
                    self.push(c);
                }
                _ if self.expect == Expect::Value => self.read_value(),
                c => {
                    // A key character (or stray token): deser-hjson accepts
                    // unquoted keys, so pass it through unchanged.
                    self.bump();
                    self.push(c);
                }
            }
        }
        self.out
    }

    /// Read a quoteless value: everything up to the inline terminator (`,`,
    /// `}`, `]`) or end of line, as Hjson does, then quote it unless it is a
    /// number or `true`/`false`/`null`. Trailing whitespace is preserved
    /// outside the quotes so the terminator still lands where deser-hjson
    /// expects it.
    fn read_value(&mut self) {
        let mut raw = String::new();
        while let Some(c) = self.peek() {
            if matches!(c, ',' | '}' | ']' | '\n' | '\r') {
                break;
            }
            raw.push(c);
            self.bump();
        }
        let trimmed = raw.trim_end();
        let trailing = raw.get(trimmed.len()..).unwrap_or("");
        if is_bare_literal(trimmed) {
            self.out.push_str(trimmed);
        } else {
            self.push('"');
            self.out.push_str(trimmed);
            self.push('"');
        }
        self.out.push_str(trailing);
        self.expect = Expect::Other;
    }

    fn open(&mut self, delim: char, is_object: bool, next: Expect) {
        self.bump();
        self.push(delim);
        self.stack.push(is_object);
        self.expect = next;
    }

    fn close(&mut self) {
        if let Some(ch) = self.bump() {
            self.push(ch);
        }
        self.stack.pop();
        self.expect = Expect::Other;
    }

    fn copy_double_string(&mut self) {
        self.push('"');
        self.bump();
        while let Some(ch) = self.bump() {
            self.push(ch);
            if ch == '\\' {
                if let Some(next) = self.bump() {
                    self.push(next);
                }
            } else if ch == '"' {
                break;
            }
        }
        self.after_scalar();
    }

    fn copy_quote(&mut self) {
        if self.peek_at(1) == Some('\'') && self.peek_at(2) == Some('\'') {
            self.copy_triple_string();
        } else {
            self.copy_single_string();
        }
        self.after_scalar();
    }

    fn copy_triple_string(&mut self) {
        for _ in 0..3 {
            if let Some(ch) = self.bump() {
                self.push(ch);
            }
        }
        loop {
            match self.peek() {
                None => break,
                Some('\'') if self.peek_at(1) == Some('\'') && self.peek_at(2) == Some('\'') => {
                    for _ in 0..3 {
                        if let Some(ch) = self.bump() {
                            self.push(ch);
                        }
                    }
                    break;
                }
                Some(ch) => {
                    self.bump();
                    self.push(ch);
                }
            }
        }
    }

    fn copy_single_string(&mut self) {
        self.push('\'');
        self.bump();
        while let Some(ch) = self.bump() {
            self.push(ch);
            if ch == '\\' {
                if let Some(next) = self.bump() {
                    self.push(next);
                }
            } else if ch == '\'' {
                break;
            }
        }
    }

    fn copy_line_comment(&mut self) {
        while let Some(ch) = self.peek() {
            if ch == '\n' {
                break;
            }
            self.bump();
            self.push(ch);
        }
    }

    fn copy_block_comment(&mut self) {
        // Consume `/*`.
        for _ in 0..2 {
            if let Some(ch) = self.bump() {
                self.push(ch);
            }
        }
        while let Some(ch) = self.bump() {
            self.push(ch);
            if ch == '*' && self.peek() == Some('/') {
                if let Some(slash) = self.bump() {
                    self.push(slash);
                }
                break;
            }
        }
    }

    fn after_scalar(&mut self) {
        if self.expect == Expect::Value {
            self.expect = Expect::Other;
        }
    }
}

/// Whether a trimmed quoteless value is a JSON literal that must stay unquoted
/// (a number, or `true`/`false`/`null`). Everything else is a string.
fn is_bare_literal(value: &str) -> bool {
    matches!(value, "true" | "false" | "null") || is_json_number(value)
}

/// Whether `value` is a number in the exact RFC 8259 JSON grammar:
///
/// ```text
/// number = [ "-" ] int [ frac ] [ exp ]
/// int    = "0" | ( digit1-9 *DIGIT )
/// frac   = "." 1*DIGIT
/// exp    = ("e" | "E") [ "+" | "-" ] 1*DIGIT
/// ```
///
/// `f64::parse` is far more permissive than JSON — it accepts leading zeros
/// (`007`), a leading `+` (`+5`), a bare fraction (`.5`), a trailing dot (`1.`),
/// and `inf`/`nan`. Quoting those spellings would be wrong (they must stay
/// strings), and leaving them unquoted would smuggle non-JSON numbers into the
/// decoded value, so the grammar is matched exactly here.
fn is_json_number(value: &str) -> bool {
    let bytes = value.as_bytes();
    let digit_at = |i: usize| bytes.get(i).is_some_and(u8::is_ascii_digit);
    let mut i = 0;

    if bytes.get(i) == Some(&b'-') {
        i += 1;
    }

    // int = "0" | digit1-9 *DIGIT
    match bytes.get(i) {
        Some(&b'0') => i += 1,
        Some(c) if c.is_ascii_digit() => {
            i += 1;
            while digit_at(i) {
                i += 1;
            }
        }
        _ => return false,
    }

    // frac = "." 1*DIGIT
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        if !digit_at(i) {
            return false;
        }
        while digit_at(i) {
            i += 1;
        }
    }

    // exp = ("e" | "E") ["+" | "-"] 1*DIGIT
    if matches!(bytes.get(i), Some(&b'e' | &b'E')) {
        i += 1;
        if matches!(bytes.get(i), Some(&b'+' | &b'-')) {
            i += 1;
        }
        if !digit_at(i) {
            return false;
        }
        while digit_at(i) {
            i += 1;
        }
    }

    i == bytes.len()
}

/// Rewrite value-position barewords in Hjson `input` to quoted strings.
#[must_use]
pub fn normalize(input: &str) -> String {
    Relaxer::new(input).run()
}
