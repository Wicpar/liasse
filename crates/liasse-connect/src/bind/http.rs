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

/// A parsed request: method, lower-cased headers, and the body bytes. The path is
/// parsed for well-formedness but not retained — the binding routes by method (POST
/// requests, GET the SSE stream).
pub struct Request {
    /// The request method (`GET`, `POST`).
    pub method: String,
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
        // The path is required in the request line but not retained.
        parts.next().ok_or(HttpError::BadRequest)?;

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
        Ok(Self { method, headers, body })
    }

    /// A header value by lower-cased name.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
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
