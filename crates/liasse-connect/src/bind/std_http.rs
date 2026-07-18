//! The reference transport binding: blocking `std::net` HTTP/1.1 + SSE (§12).
//!
//! One actor thread owns the [`ConnectCore`] and serves it single-threaded; a handler
//! thread per socket parses one request, hands the core a [`Job`] over an `mpsc`
//! channel, and writes the reply. The core is built *inside* the actor thread from a
//! factory, so it never crosses a thread boundary — only the plain owned [`Job`]s and
//! replies do. POSTs carry the tagged [`Upstream`] request body; a GET opens the
//! anonymous downstream SSE stream, which stays OPEN and streams frames as they are
//! produced.
//!
//! # Anonymous, unstealable stream binding (security)
//! The SSE `GET` is opened ANONYMOUSLY — no cookie, no capability in the URL. The core
//! mints a fresh high-entropy **ephemeral stream-session id** and announces it as the
//! socket's FIRST event (`event: liasse-session`, `data: {"stream":"<id>"}`), kept
//! DISTINCT from the default `message` events that carry §12.2 wire frames. The socket
//! is registered UNBOUND: it receives no data. A subscription's frames flow only after a
//! `view` POST that carries BOTH the connection credential (`Liasse-Connection`, `C`) and
//! the session id (`Liasse-Stream`): the core verifies `C`, BINDS the session to `C` on
//! first authenticated use, and thereafter only `C` may attach to that socket. So the
//! session id (delivered only in-band on the victim's own socket) is worthless without
//! `C`, and a stolen `C` cannot attach to a victim's socket. The actor forwards each
//! bound connection's produced frames to its socket's writer over a bounded channel; a
//! writer that stalls or dies is dropped, and the client reconnects to a fresh session.

use std::collections::BTreeMap;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Sender, SyncSender, TrySendError};
use std::thread;

use liasse_store::InstanceStore;
use liasse_wire::serde_json::json;
use liasse_wire::{ConnectionToken, Downstream, FaultCode, OperationId, SseEvent, Upstream};

use crate::core::{ConnectCore, Reply, StreamSession};
use crate::error::ConnectError;

use super::http::{write_response, write_sse_event, write_sse_head, Request};

/// The bound on frames buffered toward one SSE writer before the socket is dropped as
/// too slow (D3 backpressure): the actor never blocks, and a dropped socket reconnects
/// to a fresh session with a fresh init.
const SSE_WRITER_BOUND: usize = 256;

/// One unit of work for the actor thread. Each carries the reply channel the handler
/// waits on (except the fire-and-forget stream teardown).
enum Job {
    /// A GET opened an anonymous SSE socket: mint + announce its ephemeral session, and
    /// register the writer channel so produced frames can be forwarded to it.
    OpenStream {
        events: SyncSender<SseEvent>,
        reply: Sender<StreamSession>,
    },
    /// Dispatch a decoded request (§12.1). The frame is boxed so the job kinds stay a
    /// similar size. `stream` is the ephemeral session the POST binds/routes onto.
    Submit {
        conn: Option<ConnectionToken>,
        stream: Option<StreamSession>,
        operation: Option<OperationId>,
        frame: Box<Upstream>,
        reply: Sender<Result<Reply, ConnectError>>,
    },
    /// An SSE writer ended (socket closed): forget the session so nothing targets it.
    CloseStream { session: StreamSession },
}

/// A running reference server: its bound address and the actor channel keeping it alive.
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

/// Serve a [`ConnectCore`] over `listener`. The core is constructed by `factory` inside
/// the actor thread, so it need not be `Send`; only requests and replies cross threads.
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
    thread::spawn(move || run_actor(factory(), &rx));
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

/// The single-threaded actor: it owns the core and the live SSE writer channels, and is
/// the only thing that ever touches either.
fn run_actor<S: InstanceStore>(mut core: ConnectCore<S>, rx: &mpsc::Receiver<Job>) {
    let mut sockets: BTreeMap<StreamSession, SyncSender<SseEvent>> = BTreeMap::new();
    for job in rx {
        match job {
            Job::OpenStream { events, reply } => {
                let session = core.open_stream();
                let announcement = core.stream_announcement(&session);
                if events.try_send(announcement).is_ok() {
                    sockets.insert(session.clone(), events);
                } else {
                    core.close_stream(&session);
                }
                let _ = reply.send(session);
            }
            Job::Submit { conn, stream, operation, frame, reply } => {
                let result = dispatch(&mut core, conn.as_ref(), stream.as_ref(), operation, *frame);
                let _ = reply.send(result);
                deliver(&mut core, &mut sockets);
            }
            Job::CloseStream { session } => {
                sockets.remove(&session);
                core.close_stream(&session);
            }
        }
    }
}

/// Bind the stream-session (theft check) then dispatch the frame. Binding first means a
/// `view`'s init lands on the just-bound socket; a theft (a `C` presenting another
/// connection's session) faults before the frame runs.
fn dispatch<S: InstanceStore>(
    core: &mut ConnectCore<S>,
    conn: Option<&ConnectionToken>,
    stream: Option<&StreamSession>,
    operation: Option<OperationId>,
    frame: Upstream,
) -> Result<Reply, ConnectError> {
    if let (Some(conn), Some(stream)) = (conn, stream) {
        core.bind_stream(conn, stream)?;
    }
    core.submit(conn, operation, frame)
}

/// Forward every bound connection's freshly produced frames to its SSE writer. A writer
/// that is full (too slow) or gone is dropped and its session forgotten (D3); the client
/// reconnects to a fresh session. The actor never blocks — `try_send` returns at once.
fn deliver<S: InstanceStore>(
    core: &mut ConnectCore<S>,
    sockets: &mut BTreeMap<StreamSession, SyncSender<SseEvent>>,
) {
    let mut dead = Vec::new();
    for (session, batch) in core.take_stream_deliveries() {
        let Some(writer) = sockets.get(&session) else { continue };
        for event in batch {
            match writer.try_send(event) {
                Ok(()) => {}
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                    dead.push(session.clone());
                    break;
                }
            }
        }
    }
    for session in dead {
        sockets.remove(&session);
        core.close_stream(&session);
    }
}

/// Parse one request off `stream` and write its response.
fn handle(mut stream: TcpStream, tx: &Sender<Job>) {
    match Request::read(&mut stream) {
        Ok(request) => {
            let _ = route(&mut stream, tx, &request);
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
fn route(stream: &mut TcpStream, tx: &Sender<Job>, request: &Request) -> std::io::Result<()> {
    match request.method.as_str() {
        "GET" => serve_stream(stream, tx),
        "POST" => serve_submit(stream, tx, request),
        _ => write_response(stream, 404, "text/plain", b"not found", &[]),
    }
}

/// Serve the anonymous SSE stream (GET). No connection or session is presented: the core
/// mints a fresh ephemeral session, announces it as the first event, and the socket then
/// streams whatever frames the actor forwards to it (nothing until a `view` binds it).
fn serve_stream(stream: &mut TcpStream, tx: &Sender<Job>) -> std::io::Result<()> {
    write_sse_head(stream)?;
    let (events_tx, events_rx) = mpsc::sync_channel::<SseEvent>(SSE_WRITER_BOUND);
    let (reply_tx, reply_rx) = mpsc::channel::<StreamSession>();
    if tx.send(Job::OpenStream { events: events_tx, reply: reply_tx }).is_err() {
        return Ok(());
    }
    let Ok(session) = reply_rx.recv() else {
        return Ok(());
    };
    // Stream events until the peer goes away (write fails) or the actor drops the writer.
    for event in events_rx {
        if write_sse_event(stream, &event).is_err() {
            break;
        }
    }
    let _ = tx.send(Job::CloseStream { session });
    Ok(())
}

/// Serve a request submission (POST): decode the tagged frame body and its capability
/// headers, dispatch (binding the stream-session first), and write the reply.
fn serve_submit(stream: &mut TcpStream, tx: &Sender<Job>, request: &Request) -> std::io::Result<()> {
    let Ok(body) = std::str::from_utf8(&request.body) else {
        return fault(stream, 400, FaultCode::Malformed, "body is not UTF-8");
    };
    let frame: Upstream = match liasse_wire::decode(body) {
        Ok(frame) => frame,
        Err(_) => return fault(stream, 400, FaultCode::Malformed, "frame did not parse"),
    };
    let conn = connection_of(request);
    let session = stream_of(request);
    let operation = request.header("liasse-operation-id").map(OperationId::new);
    let (reply_tx, reply_rx) = mpsc::channel();
    let job = Job::Submit { conn, stream: session, operation, frame: Box::new(frame), reply: reply_tx };
    if tx.send(job).is_err() {
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
            // The body carries the connection capability; the client presents it as the
            // `Liasse-Connection` header on every later request. No cookie is set — the
            // SSE stream is bound by the in-band ephemeral session, not an ambient
            // credential. The header echo suits a non-browser client that reads it there.
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

/// The connection capability a POST presents (`Liasse-Connection`). It is a bearer
/// credential, so it rides a request header — never a URL or a cookie. Absent yields
/// `None`, taking the no-connection path.
fn connection_of(request: &Request) -> Option<ConnectionToken> {
    request.header("liasse-connection").map(ConnectionToken::new)
}

/// The ephemeral stream-session a POST binds/routes onto (`Liasse-Stream`), echoed back
/// from the socket's first-event announcement. Absent yields `None` — a POST that opens
/// no subscription (e.g. `hello`) needs no session.
fn stream_of(request: &Request) -> Option<StreamSession> {
    request.header("liasse-stream").map(StreamSession::new)
}
