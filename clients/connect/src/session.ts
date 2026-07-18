//! `connect(baseUrl, hello)` and the `Session` it returns — the ergonomics layer over
//! the wasm core and the two transports.
//!
//! The session owns exactly one connection: one `WireClient` replica, one anonymous SSE
//! stream, and the set of live subscriptions. It holds NO authority — every capability
//! (connection token, operation id) is opaque data it attaches to a request and echoes
//! back. The stream is anonymous: the server announces a fresh ephemeral stream-session
//! id on the stream's first event, and the session binds each subscription to it with an
//! authenticated `view` POST — so nothing presentable can steal a stream. It is a "dumb"
//! client: on a new stream-session (every (re)connect) it re-binds its subscriptions; on
//! a §12.2 `reset` it re-establishes them; on a `close` it surfaces the reason; on a
//! `fault` it delivers a handled error. No frame ever throws out of the stream callback.

import {
  parseApplied,
  parseFetched,
  parseHelloConnection,
  parseManifest,
  parseOpened,
  parseOutcome,
} from "./boundary.js";
import { ConnectError, FaultError, toConnectError } from "./errors.js";
import { HttpTransport } from "./http.js";
import { Stream, defaultEventSourceFactory } from "./stream.js";
import { Subscription } from "./subscription.js";
import type { ViewIntent } from "./subscription.js";
import { asOperationId, asSubId } from "./types.js";
import type {
  Applied,
  CallOptions,
  ConnectionState,
  ConnectOptions,
  ConnectionToken,
  EventSourceFactory,
  FetchLike,
  Hello,
  Json,
  Outcome,
  Schedule,
  StreamSession,
  SubId,
  SubscribeOptions,
  WireClientCore,
  WireCore,
} from "./types.js";
import { loadCore } from "./wasm.js";

/// A session-level error listener (faults not tied to a single subscription).
export type SessionErrorListener = (error: ConnectError) => void;

/// A listener over the downstream connection's state (connecting/reconnecting/…).
export type ConnectionStateListener = (state: ConnectionState) => void;

/// One logical connection to a Liasse server (§12.3 coherence unit).
export class Session {
  private readonly core: WireCore;
  private readonly client: WireClientCore;
  private readonly http: HttpTransport;
  private readonly streamFactory: EventSourceFactory;
  private readonly schedule: Schedule | undefined;
  private readonly baseUrl: string;
  private readonly connection: ConnectionToken;
  private readonly subs = new Map<SubId, Subscription>();
  private readonly errorListeners = new Set<SessionErrorListener>();
  private readonly stateListeners = new Set<ConnectionStateListener>();
  private readonly sessionWaiters = new Set<(session: StreamSession) => void>();
  private streamState: ConnectionState = "connecting";
  private streamSession: StreamSession | undefined;
  private stream: Stream | undefined;

  constructor(
    core: WireCore,
    http: HttpTransport,
    streamFactory: EventSourceFactory,
    schedule: Schedule | undefined,
    baseUrl: string,
    connection: ConnectionToken,
  ) {
    this.core = core;
    this.client = new core.WireClient();
    this.http = http;
    this.streamFactory = streamFactory;
    this.schedule = schedule;
    this.baseUrl = baseUrl;
    this.connection = connection;
  }

  /// The connection capability the server minted for this session.
  get connectionToken(): ConnectionToken {
    return this.connection;
  }

  /// Open a live subscription over `address` (§12.2) and return its store-like handle.
  /// Non-blocking: the handle starts empty and fills as `init`/`patch` frames arrive.
  /// Await `handle.ready` to know the server accepted the view.
  subscribe(address: string, options: SubscribeOptions = {}): Subscription {
    const sub = asSubId(newId());
    const intent: ViewIntent = {
      address,
      params: options.params ?? null,
      window: (options.window as Json | undefined) ?? null,
      auth: options.auth ?? null,
      context: options.context ?? null,
    };
    const subscription = new Subscription(sub, intent, this.client, () => this.endSubscription(sub));
    this.subs.set(sub, subscription);
    this.ensureStream();
    subscription.ready = this.bindView(subscription);
    return subscription;
  }

  /// Invoke a call (§10, §12.3). The operation capability is client-seeded — from
  /// `options.operationId` or a fresh `crypto.randomUUID()` — and carried as the
  /// `Liasse-Operation-Id` header, making the submission at-most-once.
  async call(address: string, args: Json = null, options: CallOptions = {}): Promise<Outcome> {
    const handle = new this.core.OperationHandle(options.operationId ?? newId());
    try {
      const body = this.core.encodeCall(address, args, options.auth ?? null, options.context ?? null);
      const reply = await this.http.post(body, { operationId: asOperationId(handle.id) });
      return parseOutcome(reply);
    } finally {
      handle.free();
    }
  }

  /// Read a value once at the current frontier (§12.1) — a snapshot, not a
  /// subscription. Returns the projected value verbatim.
  async fetch(address: string, params: Json = null): Promise<Json> {
    const reply = await this.http.post(this.core.encodeFetch(address, params));
    return parseFetched(reply);
  }

  /// Query the retained status of an operation by its capability (§12.3).
  async operation(id: string): Promise<Outcome> {
    const reply = await this.http.post(this.core.encodeOperation(id));
    return parseOutcome(reply);
  }

  /// Request the app's exposed manifest (§12.1).
  async manifest(): Promise<Json> {
    const reply = await this.http.post(this.core.encodeManifest());
    return parseManifest(reply);
  }

  /// Listen for session-level errors (faults not tied to one subscription). Returns a
  /// detach function.
  onError(listener: SessionErrorListener): () => void {
    this.errorListeners.add(listener);
    return () => this.errorListeners.delete(listener);
  }

  /// The downstream connection's current state.
  get connectionState(): ConnectionState {
    return this.streamState;
  }

  /// Observe the downstream connection state (connecting/open/reconnecting/closed), so
  /// an app can show a "reconnecting…" indicator on a bad network. Fires immediately
  /// with the current state and returns a detach function.
  onConnectionState(listener: ConnectionStateListener): () => void {
    this.stateListeners.add(listener);
    listener(this.streamState);
    return () => this.stateListeners.delete(listener);
  }

  /// Close the connection: stop the stream and release the replica. Subscriptions stop
  /// receiving frames.
  close(): void {
    this.stream?.close();
    this.stream = undefined;
    this.subs.clear();
    this.client.free();
  }

  // --- internals ---------------------------------------------------------------------

  private ensureStream(): void {
    if (this.stream !== undefined) {
      return;
    }
    this.stream = new Stream(this.streamFactory, this.baseUrl, this.schedule);
    this.stream.open({
      onFrame: (data, frontier) => this.handleFrame(data, frontier),
      onSession: (session) => this.onSession(session),
      onState: (state) => this.emitState(state),
    });
  }

  /// A fresh ephemeral stream-session was announced on the stream's first event. Record
  /// it (so bind POSTs carry it) and, if this is a RECONNECT (a session already existed),
  /// re-bind every live subscription to the new socket — the §12.2 `init` frames that
  /// follow re-establish their rows.
  private onSession(session: StreamSession): void {
    const reconnected = this.streamSession !== undefined;
    this.streamSession = session;
    this.http.setStream(session);
    for (const resolve of this.sessionWaiters) {
      resolve(session);
    }
    this.sessionWaiters.clear();
    if (reconnected) {
      this.bindAllLive();
    }
  }

  /// Resolve once the stream has announced its session; a subscribe issued before the
  /// stream connected waits here rather than binding to nothing.
  private requireStreamSession(): Promise<StreamSession> {
    if (this.streamSession !== undefined) {
      return Promise.resolve(this.streamSession);
    }
    return new Promise((resolve) => this.sessionWaiters.add(resolve));
  }

  private emitState(state: ConnectionState): void {
    this.streamState = state;
    for (const listener of this.stateListeners) {
      listener(state);
    }
  }

  /// Fold one raw downstream frame through the wasm replica and route its effect. A
  /// hostile frame the core refuses becomes a handled error here — never a throw out
  /// of the stream callback (AGENTS.md).
  private handleFrame(data: string, frontier: string): void {
    let applied: Applied;
    try {
      applied = parseApplied(this.client.applyFrame(data, frontier));
    } catch (thrown) {
      this.emitError(toConnectError(thrown, "invalid downstream frame"));
      return;
    }
    this.route(applied);
  }

  private route(applied: Applied): void {
    switch (applied.kind) {
      case "init":
      case "patch":
      case "scalar":
      case "close":
        this.subs.get(applied.sub)?.notify();
        return;
      case "frontier":
        for (const subscription of this.subs.values()) {
          subscription.notify();
        }
        return;
      case "reset":
        this.bindAllLive();
        return;
      case "fault":
        this.emitError(new FaultError(applied.fault.code, applied.fault.message));
        return;
    }
  }

  /// Re-bind every live subscription to the current stream (used on a §12.2 `reset`, and
  /// on a reconnect's new stream-session). The `init` frames that follow re-establish the
  /// rows; a subscription the server already closed is left terminated.
  private bindAllLive(): void {
    for (const subscription of this.subs.values()) {
      if (!subscription.closed) {
        subscription.ready = this.bindView(subscription);
      }
    }
  }

  /// POST a `view` binding a subscription to the current stream-session (§12.2). Marks
  /// the subscription as re-opening first (so its status returns to `pending` — after a
  /// `reset` the replica is cleared and must not read as a stale `open`), waits for the
  /// stream-session, and resolves when the view opened. On failure it moves the
  /// subscription to `failed` with the error IN its state AND delivers it to the error
  /// listeners, then rejects (the rejection is always observed).
  private bindView(subscription: Subscription): Promise<void> {
    subscription.beginOpen();
    const opened = this.postView(subscription);
    opened.catch((thrown) => subscription.fail(toConnectError(thrown, "view failed")));
    return opened;
  }

  private async postView(subscription: Subscription): Promise<void> {
    await this.requireStreamSession();
    const { address, params, window, auth, context } = subscription.intent;
    const body = this.core.encodeView(subscription.sub, address, params, window, auth, context);
    parseOpened(await this.http.post(body));
  }

  private async endSubscription(sub: SubId): Promise<void> {
    const subscription = this.subs.get(sub);
    if (subscription === undefined) {
      return;
    }
    try {
      await this.http.post(this.core.encodeUnsubscribe(sub));
    } finally {
      // The server closes the subscription on the stream; drop our local handle.
      this.subs.delete(sub);
    }
  }

  private emitError(error: ConnectError): void {
    for (const listener of this.errorListeners) {
      listener(error);
    }
    for (const subscription of this.subs.values()) {
      subscription.deliverError(error);
    }
  }
}

/// Open a connection (§11) and return its session. `hello` optionally authenticates
/// the connection; `options` injects the wasm core and transports (the defaults suit a
/// browser: the web wasm core, `globalThis.fetch`, and a native `EventSource`).
export async function connect(baseUrl: string, hello: Hello = {}, options: ConnectOptions = {}): Promise<Session> {
  const core = options.core ?? (await loadCore());
  const fetchLike = options.fetch ?? defaultFetch;
  const streamFactory = options.eventSource ?? defaultEventSourceFactory;

  const http = new HttpTransport(baseUrl, fetchLike);
  const reply = await http.post(core.encodeHello(hello.auth ?? null, hello.context ?? null));
  const connection = parseHelloConnection(reply);
  http.setConnection(connection);

  return new Session(core, http, streamFactory, options.schedule, baseUrl, connection);
}

/// The default POST transport: the platform `fetch`. Node 18+ and every browser
/// provide it; a non-fetch host injects `ConnectOptions.fetch`.
const defaultFetch: FetchLike = (url, init) => fetch(url, init);

/// A high-entropy client-seeded capability (subscription id, operation id).
function newId(): string {
  return crypto.randomUUID();
}
