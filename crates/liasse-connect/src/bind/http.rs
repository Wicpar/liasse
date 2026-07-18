//! A minimal, bounded HTTP/1.1 request reader and response writer for the reference
//! binding — no dependency, strict, and non-panicking on hostile bytes.
//!
//! This is deliberately just enough to carry the connector's request/SSE shapes: a
//! request line, header lines, and a `Content-Length` body, each bounded so a slow
//! or oversized peer cannot exhaust memory. Anything it cannot parse is a
//! [`HttpError`], never a panic (AGENTS.md).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;

/// The largest header block and body the reference binding accepts.
const MAX_HEADERS: usize = 16 * 1024;
const MAX_BODY: usize = 1024 * 1024;

/// Why an inbound HTTP request could not be read.
#[derive(Debug)]
pub enum HttpError {
    /// The request line or headers were malformed.
    BadRequest,
    /// The header block or body exceeded its bound.
    TooLarge,
    /// An I/O error on the socket.
    Io,
}

impl From<std::io::Error> for HttpError {
    fn from(_: std::io::Error) -> Self {
        Self::Io
    }
}

/// A parsed request: method, the target's query string, lower-cased headers, and the
/// body bytes. The path is parsed for well-formedness but not retained — the binding
/// routes by method (POST requests, GET the SSE stream); only the query string is kept,
/// because the non-secret resume cursor may ride it (§12.2).
pub struct Request {
    /// The request method (`GET`, `POST`).
    pub method: String,
    /// The request target's query string (the part after `?`), if any.
    pub query: Option<String>,
    /// Header fields, keyed by their lower-cased name.
    pub headers: BTreeMap<String, String>,
    /// The request body.
    pub body: Vec<u8>,
}

impl Request {
    /// Read one bounded request from `stream`.
    pub fn read(stream: &mut TcpStream) -> Result<Self, HttpError> {
        let mut reader = BufReader::new(stream);

        let mut request_line = String::new();
        let read = reader.by_ref().take(MAX_HEADERS as u64).read_line(&mut request_line)?;
        if read == 0 {
            return Err(HttpError::BadRequest);
        }
        let mut parts = request_line.split_whitespace();
        let method = parts.next().ok_or(HttpError::BadRequest)?.to_owned();
        // The request target is required. Its query string is retained (the §12.2
        // resume cursor may ride it); the path is not — the binding routes by method.
        let target = parts.next().ok_or(HttpError::BadRequest)?;
        let query = target.split_once('?').map(|(_, q)| q.to_owned());

        let mut headers = BTreeMap::new();
        let mut header_bytes = request_line.len();
        loop {
            let mut line = String::new();
            let n = reader.by_ref().take(MAX_HEADERS as u64).read_line(&mut line)?;
            if n == 0 {
                return Err(HttpError::BadRequest);
            }
            header_bytes += n;
            if header_bytes > MAX_HEADERS {
                return Err(HttpError::TooLarge);
            }
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                break;
            }
            if let Some((name, value)) = trimmed.split_once(':') {
                headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
            }
        }

        let length: usize = headers
            .get("content-length")
            .map_or(Ok(0), |value| value.parse().map_err(|_| HttpError::BadRequest))?;
        if length > MAX_BODY {
            return Err(HttpError::TooLarge);
        }
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body)?;
        Ok(Self { method, query, headers, body })
    }

    /// A header value by lower-cased name.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    /// The value of cookie `name` from the untrusted `Cookie` request header, if
    /// present and well-formed. A browser's native `EventSource` can send no custom
    /// header, so the connection capability rides this ambient cookie instead. Parsing
    /// is total: a malformed or absent header simply yields `None`, never a panic
    /// (AGENTS.md — bound and sanitize hostile input at the boundary).
    #[must_use]
    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.header("cookie")?.split(';').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key.trim() == name).then(|| value.trim())
        })
    }

    /// A named URL query parameter from the request target, percent-decoded. The
    /// resume cursor (`last-event-id`) is a non-secret frontier token, so it may ride
    /// the URL; the connection capability never does (a capability in a URL leaks via
    /// history, access logs, and `Referer`). Absent if the target had no such key.
    #[must_use]
    pub fn query_param(&self, name: &str) -> Option<String> {
        let query = self.query.as_deref()?;
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == name).then(|| percent_decode(value))
        })
    }
}

/// Percent-decode a URL query value, matching the client's `encodeURIComponent`. An
/// incomplete or non-hex `%` sequence is kept verbatim, so decoding is total and never
/// panics or indexes on hostile input (AGENTS.md). Frontier tokens under the default
/// minter need no decoding; this keeps the resume cursor correct across the token seam.
fn percent_decode(value: &str) -> String {
    let mut out: Vec<u8> = Vec::with_capacity(value.len());
    let mut rest = value.as_bytes();
    while let Some((&first, tail)) = rest.split_first() {
        if first == b'%'
            && let Some((&hi, &lo, remainder)) = two(tail)
            && let (Some(high), Some(low)) = (hex_nibble(hi), hex_nibble(lo))
        {
            out.push((high << 4) | low);
            rest = remainder;
        } else {
            out.push(first);
            rest = tail;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The first two bytes of `bytes` and the remainder, without indexing.
fn two(bytes: &[u8]) -> Option<(&u8, &u8, &[u8])> {
    let (hi, tail) = bytes.split_first()?;
    let (lo, remainder) = tail.split_first()?;
    Some((hi, lo, remainder))
}

/// The numeric value of a single hex digit, or `None` if the byte is not one.
fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Write a complete HTTP response with a body.
pub fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    extra: &[(&str, &str)],
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        reason(status),
        body.len(),
    );
    for (name, value) in extra {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

/// Write an SSE response head, then the pre-rendered event stream, then close. The
/// reference binding delivers the buffered/replayed stream and closes; the client
/// reconnects with `Last-Event-ID` to resume (§12.2).
pub fn write_sse(stream: &mut TcpStream, events: &str) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        events.len(),
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(events.as_bytes())?;
    stream.flush()
}

/// The canonical reason phrase for the small set of statuses the binding returns.
fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        _ => "Status",
    }
}
