//! The reference transport binding: blocking `std::net` HTTP/1.1 + SSE (§12).
//!
//! One actor thread owns the [`ConnectCore`] and serves it single-threaded; a handler
//! thread per socket parses one request, hands the core a [`Job`] over an `mpsc`
//! channel, and writes the reply. The core is built *inside* the actor thread from a
//! factory, so it never crosses a thread boundary — only the plain owned [`Job`]s and
//! replies do. POSTs carry the tagged [`Upstream`] request body; a GET is the SSE
//! stream, resuming from `Last-Event-ID`. This binding delivers the retained/replayed
//! stream and closes; the client reconnects to resume (§12.2) — enough for the
//! reference and the socket smoke, while an axum adapter (a later stage) can hold the
//! stream open.

use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Sender};
use std::thread;

use liasse_store::InstanceStore;
use liasse_wire::serde_json::json;
use liasse_wire::{
    ConnectionToken, Downstream, FaultCode, OperationId, SseEvent, Upstream,
};

use crate::core::{ConnectCore, Reply};
use crate::error::ConnectError;

use super::http::{Request, write_response, write_sse};

/// One unit of work for the actor thread: a request to dispatch, or an SSE (re)connect
/// to serve. Each carries the reply channel the handler waits on.
enum Job {
    /// Dispatch a decoded request (§12.1). The frame is boxed so the two job kinds
    /// stay a similar size.
    Submit {
        conn: Option<ConnectionToken>,
        operation: Option<OperationId>,
        frame: Box<Upstream>,
        reply: Sender<Result<Reply, ConnectError>>,
    },
    /// (Re)connect the SSE stream, resuming from `last` (§12.2).
    Resume {
        conn: ConnectionToken,
        last: Option<String>,
        reply: Sender<Result<Vec<SseEvent>, ConnectError>>,
    },
}

/// A running reference server: its bound address and the actor channel keeping it
/// alive.
pub struct Server {
    addr: std::net::SocketAddr,
    _actor: Sender<Job>,
}

impl Server {
    /// The address the server is bound to.
    #[must_use]
    pub fn local_addr(&self) -> std::net::SocketAddr {
        self.addr
    }
}

/// Serve a [`ConnectCore`] over `listener`. The core is constructed by `factory`
/// inside the actor thread, so it need not be `Send`; only requests and replies cross
/// threads.
///
/// # Errors
/// Propagates a failure to read the listener's local address.
pub fn serve<S, F>(listener: TcpListener, factory: F) -> std::io::Result<Server>
where
    S: InstanceStore + 'static,
    F: FnOnce() -> ConnectCore<S> + Send + 'static,
{
    let addr = listener.local_addr()?;
    let (tx, rx) = mpsc::channel::<Job>();
    thread::spawn(move || {
        let mut core = factory();
        for job in rx {
            serve_job(&mut core, job);
        }
    });
    let accept_tx = tx.clone();
    thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(stream) = incoming else { continue };
            let job_tx = accept_tx.clone();
            thread::spawn(move || handle(stream, &job_tx));
        }
    });
    Ok(Server { addr, _actor: tx })
}

/// Run one job against the core.
fn serve_job<S: InstanceStore>(core: &mut ConnectCore<S>, job: Job) {
    match job {
        Job::Submit { conn, operation, frame, reply } => {
            let _ = reply.send(core.submit(conn.as_ref(), operation, *frame));
        }
        Job::Resume { conn, last, reply } => {
            let _ = reply.send(core.resume(&conn, last.as_deref()));
        }
    }
}

/// Parse one request off `stream` and write its response.
fn handle(mut stream: TcpStream, tx: &Sender<Job>) {
    match Request::read(&mut stream) {
        Ok(request) => {
            let _ = route(&mut stream, tx, request);
        }
        Err(super::http::HttpError::TooLarge) => {
            let _ = fault(&mut stream, 413, FaultCode::Oversized, "frame exceeds the size bound");
        }
        Err(_) => {
            let _ = fault(&mut stream, 400, FaultCode::Malformed, "request did not parse");
        }
    }
}

/// Route a parsed request to the actor and write its reply.
fn route(stream: &mut TcpStream, tx: &Sender<Job>, request: Request) -> std::io::Result<()> {
    match request.method.as_str() {
        "GET" => serve_stream(stream, tx, &request),
        "POST" => serve_submit(stream, tx, &request),
        _ => write_response(stream, 404, "text/plain", b"not found", &[]),
    }
}

/// Serve the SSE stream (GET).
fn serve_stream(stream: &mut TcpStream, tx: &Sender<Job>, request: &Request) -> std::io::Result<()> {
    let Some(conn) = request.header("liasse-connection") else {
        return fault(stream, 400, FaultCode::BadToken, "missing connection");
    };
    let conn = ConnectionToken::new(conn);
    let last = request.header("last-event-id").map(str::to_owned);
    let (reply_tx, reply_rx) = mpsc::channel();
    if tx.send(Job::Resume { conn, last, reply: reply_tx }).is_err() {
        return fault(stream, 500, FaultCode::Internal, "actor unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(events)) => write_sse(stream, &SseEvent::encode_stream(&events)),
        Ok(Err(error)) => fault(stream, status_of(&error), error.code(), &error.sanitized()),
        Err(_) => fault(stream, 500, FaultCode::Internal, "actor unavailable"),
    }
}

/// Serve a request submission (POST): decode the tagged frame body, dispatch, reply.
fn serve_submit(stream: &mut TcpStream, tx: &Sender<Job>, request: &Request) -> std::io::Result<()> {
    let Ok(body) = std::str::from_utf8(&request.body) else {
        return fault(stream, 400, FaultCode::Malformed, "body is not UTF-8");
    };
    let frame: Upstream = match liasse_wire::decode(body) {
        Ok(frame) => frame,
        Err(_) => return fault(stream, 400, FaultCode::Malformed, "frame did not parse"),
    };
    let conn = request.header("liasse-connection").map(ConnectionToken::new);
    let operation = request.header("liasse-operation-id").map(OperationId::new);
    let (reply_tx, reply_rx) = mpsc::channel();
    if tx.send(Job::Submit { conn, operation, frame: Box::new(frame), reply: reply_tx }).is_err() {
        return fault(stream, 500, FaultCode::Internal, "actor unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(reply)) => write_reply(stream, reply),
        Ok(Err(error)) => fault(stream, status_of(&error), error.code(), &error.sanitized()),
        Err(_) => fault(stream, 500, FaultCode::Internal, "actor unavailable"),
    }
}

/// Serialize a [`Reply`] to its HTTP response.
fn write_reply(stream: &mut TcpStream, reply: Reply) -> std::io::Result<()> {
    match reply {
        Reply::Hello { connection } => {
            let body = json!({ "connection": connection.as_str() }).to_string();
            write_response(
                stream,
                200,
                "application/json",
                body.as_bytes(),
                &[("Liasse-Connection", connection.as_str())],
            )
        }
        Reply::Manifest(surfaces) => {
            let body = json!({ "surfaces": surfaces }).to_string();
            write_response(stream, 200, "application/json", body.as_bytes(), &[])
        }
        Reply::Opened { frontier } => {
            let body = json!({ "frontier": frontier.as_str() }).to_string();
            write_response(stream, 200, "application/json", body.as_bytes(), &[])
        }
        Reply::Unsubscribed => write_response(stream, 200, "application/json", b"{}", &[]),
        Reply::Fetched(value) => {
            write_response(stream, 200, "application/json", value.to_string().as_bytes(), &[])
        }
        Reply::Outcome(outcome) => {
            let body = liasse_wire::encode(&outcome).unwrap_or_default();
            write_response(stream, 200, "application/json", body.as_bytes(), &[])
        }
    }
}

/// Write a transport-fault response (a downstream `fault` body).
fn fault(stream: &mut TcpStream, status: u16, code: FaultCode, message: &str) -> std::io::Result<()> {
    let frame = Downstream::Fault { code, message: message.to_owned() };
    let body = liasse_wire::encode(&frame).unwrap_or_default();
    write_response(stream, status, "application/json", body.as_bytes(), &[])
}

/// The HTTP status a transport fault is reported as.
fn status_of(error: &ConnectError) -> u16 {
    match error {
        ConnectError::NoConnection => 404,
        ConnectError::BadToken => 403,
        ConnectError::Codec(_) => 400,
        ConnectError::Oversized { .. } => 413,
        ConnectError::Host(_) => 500,
    }
}
