//! Decoding of string tokens: JSON-style escapes in quoted strings and Hjson
//! de-indentation of `'''` multiline blocks. Both the document and expression
//! grammars share this, so it lives at the crate root.

/// A raw string token, ready to be decoded into its logical text.
#[derive(Debug, Clone, Copy)]
pub(crate) enum RawString<'a> {
    /// A quoted string, including its surrounding quote characters.
    Quoted(&'a str),
    /// A `'''` multiline block: the body between the markers, plus the column
    /// (in chars) of the opening `'''`, which sets the de-indent gutter.
    Multiline {
        /// The text between the opening and closing `'''`.
        body: &'a str,
        /// The char column of the opening `'''` on its line.
        gutter: usize,
    },
}

impl RawString<'_> {
    /// The decoded logical text.
    pub(crate) fn decode(self) -> String {
        match self {
            Self::Quoted(token) => Self::decode_quoted(token),
            Self::Multiline { body, gutter } => Self::decode_multiline(body, gutter),
        }
    }

    fn decode_quoted(token: &str) -> String {
        // Strip the surrounding quotes (grammar guarantees both are present).
        let inner = token
            .strip_prefix(['"', '\''])
            .and_then(|rest| rest.strip_suffix(['"', '\'']))
            .unwrap_or(token);
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('b') => out.push('\u{8}'),
                Some('f') => out.push('\u{c}'),
                Some('/') => out.push('/'),
                Some('"') => out.push('"'),
                Some('\'') => out.push('\''),
                Some('\\') => out.push('\\'),
                Some('u') => out.push(Self::decode_unicode(&mut chars)),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        }
        out
    }

    /// Decodes a `\u` escape (the `u` already consumed), combining a UTF-16
    /// surrogate pair `𐀀` when it appears. Malformed escapes fall
    /// back to U+FFFD rather than panicking — the grammar already required four
    /// hex digits, so this only guards surrogate math.
    fn decode_unicode(chars: &mut core::str::Chars<'_>) -> char {
        let high = Self::hex4(chars);
        if (0xD800..=0xDBFF).contains(&high) {
            // Expect a following `\uXXXX` low surrogate.
            let mut lookahead = chars.clone();
            if lookahead.next() == Some('\\') && lookahead.next() == Some('u') {
                let low = Self::hex4(&mut lookahead);
                if (0xDC00..=0xDFFF).contains(&low) {
                    *chars = lookahead;
                    let combined = 0x1_0000 + ((high - 0xD800) << 10) + (low - 0xDC00);
                    return char::from_u32(combined).unwrap_or('\u{FFFD}');
                }
            }
            return '\u{FFFD}';
        }
        char::from_u32(high).unwrap_or('\u{FFFD}')
    }

    fn hex4(chars: &mut core::str::Chars<'_>) -> u32 {
        let mut value = 0u32;
        for _ in 0..4 {
            let digit = chars.next().and_then(|c| c.to_digit(16)).unwrap_or(0);
            value = value * 16 + digit;
        }
        value
    }

    fn decode_multiline(body: &str, gutter: usize) -> String {
        // Drop the first line break after the opening `'''`, per Hjson.
        let body = body
            .strip_prefix("\r\n")
            .or_else(|| body.strip_prefix('\n'))
            .unwrap_or(body);
        let mut out = String::with_capacity(body.len());
        for (index, line) in body.split('\n').enumerate() {
            if index > 0 {
                out.push('\n');
            }
            let line = line.strip_suffix('\r').unwrap_or(line);
            out.push_str(Self::deindent(line, gutter));
        }
        out
    }

    /// Removes up to `gutter` leading spaces/tabs from `line`.
    fn deindent(line: &str, gutter: usize) -> &str {
        let mut chars = line.char_indices();
        let mut end = 0;
        for _ in 0..gutter {
            match chars.next() {
                Some((offset, c)) if c == ' ' || c == '\t' => end = offset + c.len_utf8(),
                _ => break,
            }
        }
        line.get(end..).unwrap_or(line)
    }
}
