//! The wire vocabulary and transport contracts the shell speaks — as TypeScript
//! types, mirroring the Rust `liasse-wire` schema one-for-one so the boundary parses
//! into meaning rather than passing stringly-typed blobs around.
//!
//! The shell reconstructs no §12.2 semantics: these types only NAME what the wasm
//! core produces (an `Applied` effect, wire rows) and what the server returns (an
//! `Outcome`), and describe the transport seams the shell drives (fetch, EventSource).
//! Every capability the untrusted client carries is an opaque, branded string it only
//! echoes back — never a value it mints or interprets.

/// A JSON value carried verbatim from the engine's projection. The shell never
/// inspects its shape (Annex A): a row value or a scalar is opaque data.
export type Json =
  | null
  | boolean
  | number
  | string
  | Json[]
  | { [key: string]: Json };

/// A nominal string capability: two brands never interchange, mirroring the distinct
/// Rust newtypes (`ConnectionToken`, `Ft`, `Sub`, `OperationId`).
declare const brand: unique symbol;
type Branded<Tag extends string> = string & { readonly [brand]: Tag };

/// The connection capability the server mints at `hello`; presented on every request.
export type ConnectionToken = Branded<"ConnectionToken">;
/// A §12.2 frontier token (the SSE `id:`), opaque and connection-bound.
export type FrontierToken = Branded<"FrontierToken">;
/// A client-chosen subscription id, echoed on every per-subscription frame.
export type SubId = Branded<"SubId">;
/// A per-client §12.3 operation capability (idempotency key).
export type OperationId = Branded<"OperationId">;
/// An ephemeral per-socket stream-session id the server mints and announces on the
/// stream's first event. It is NOT a bearer credential — it cannot open or attach to a
/// stream on its own; it only correlates authenticated bind POSTs to this exact socket.
export type StreamSession = Branded<"StreamSession">;

/// Tag an opaque server- or client-minted string as a connection capability.
export const asConnectionToken = (value: string): ConnectionToken => value as ConnectionToken;
/// Tag a client-generated string as a subscription id.
export const asSubId = (value: string): SubId => value as SubId;
/// Tag a client-generated string as an operation capability.
export const asOperationId = (value: string): OperationId => value as OperationId;
/// Tag a server-minted string as a frontier token.
export const asFrontierToken = (value: string): FrontierToken => value as FrontierToken;
/// Tag a server-minted string as an ephemeral stream-session id.
export const asStreamSession = (value: string): StreamSession => value as StreamSession;

/// One view row on the wire (§12.2): its opaque occurrence token and exposed value.
export interface WireRow {
  /// The opaque occurrence token (`$id`), stable within the subscription.
  readonly id: string;
  /// The engine-projected value, carried verbatim.
  readonly value: Json;
}

/// Where a bounded window anchors (§12.2).
export type Anchor =
  | { readonly kind: "first" }
  | { readonly kind: "last" }
  | { readonly kind: "at"; readonly occ: string };

/// A bounded window over a row-stream view (§12.2).
export interface Window {
  /// The maximum number of rows presented.
  readonly size: number;
  /// Where the window anchors (defaults to the view's first rows server-side).
  readonly anchor?: Anchor;
  /// Whether the anchor stays centered as bounds allow.
  readonly slide?: boolean;
}

/// Why a subscription closed (§12.2) — kebab-case exactly as the wire renders it.
export type CloseReason = "unauthorized" | "unsubscribed" | "replaced" | "server-closed";
/// Why the whole connection reset (§12.2).
export type ResetReason = "unknown-connection" | "overflow" | "server-reset";
/// A stable transport-fault class distinct from a spec outcome.
export type FaultCode = "bad-token" | "malformed" | "oversized" | "internal";

/// A transport fault reported to the client: a stable class and a sanitized message.
export interface Fault {
  readonly code: FaultCode;
  readonly message: string;
}

/// The client-visible effect of folding one downstream frame — the parsed form of the
/// wasm core's `applyFrame` result, discriminated by `kind`.
export type Applied =
  | { readonly kind: "init"; readonly sub: SubId; readonly frontier: FrontierToken; readonly rows: WireRow[] }
  | { readonly kind: "patch"; readonly sub: SubId; readonly frontier: FrontierToken; readonly rows: WireRow[] }
  | { readonly kind: "scalar"; readonly sub: SubId; readonly frontier: FrontierToken; readonly scalar: Json | null }
  | { readonly kind: "close"; readonly sub: SubId; readonly closeReason: CloseReason }
  | { readonly kind: "frontier"; readonly frontier: FrontierToken }
  | { readonly kind: "reset"; readonly resetReason: ResetReason }
  | { readonly kind: "fault"; readonly fault: Fault };

/// The spec outcome of a `call`, `fetch`-adjacent, or `operation` request (§8.9, §10,
/// §11, §12.3), status-tagged exactly as `liasse-wire::Outcome` renders it.
export type Outcome =
  | { readonly status: "committed"; readonly frontier: FrontierToken; readonly commit: FrontierToken; readonly response?: Json }
  | { readonly status: "unchanged"; readonly frontier: FrontierToken; readonly response?: Json }
  | { readonly status: "rejected"; readonly code: string; readonly message: string }
  | { readonly status: "denied"; readonly code: string; readonly message: string }
  | { readonly status: "failed"; readonly code: "absent-anchor" | "scalar-view" }
  | { readonly status: "unknown" };

/// The current observable state of a subscription's replica — the store snapshot.
export interface ViewState {
  /// The subscription this state belongs to.
  readonly sub: SubId;
  /// The rows the replica holds, in view order (empty for a scalar or closed view).
  readonly rows: WireRow[];
  /// The scalar value, for a scalar/aggregate view (`null` otherwise).
  readonly scalar: Json | null;
  /// The frontier last observed for this subscription.
  readonly frontier: FrontierToken | undefined;
  /// Whether the subscription has terminated.
  readonly closed: boolean;
  /// Why it closed, if it did.
  readonly closeReason: CloseReason | undefined;
}

// --- the wasm core surface -----------------------------------------------------------
//
// These interfaces type EXACTLY the exports of `liasse-connect-wasm` (both the
// `--target web` and `--target nodejs` builds share this shape). The shell depends on
// this contract, not on a specific generated package, so a typecheck needs no build
// artifact and a test can inject either target's module.

/// The per-connection §12.2 replica the wasm core exposes. Results come back as
/// `unknown` (real JS values via `JSON.parse`) and are parsed at the boundary.
export interface WireClientCore {
  applyFrame(data: string, frontier: string): unknown;
  rows(sub: string): unknown;
  scalar(sub: string): unknown;
  frontier(sub: string): string | undefined;
  isClosed(sub: string): boolean;
  closeReason(sub: string): string | undefined;
  subs(): string[];
  connectionFrontier(): string | undefined;
  free(): void;
}

/// A wasm §12.3 operation capability holder.
export interface OperationHandleCore {
  readonly id: string;
  statusFrame(): string;
  free(): void;
}

/// The whole wasm module: the replica/handle constructors plus the request encoders.
/// The shell never re-implements any of these — they are the one source of wire truth.
export interface WireCore {
  readonly WireClient: new () => WireClientCore;
  readonly OperationHandle: new (id: string) => OperationHandleCore;
  encodeHello(auth: Json | null, context: Json | null): string;
  encodeManifest(): string;
  encodeView(
    sub: string,
    address: string,
    params: Json | null,
    window: Json | null,
    auth: Json | null,
    context: Json | null,
  ): string;
  encodeUnsubscribe(sub: string): string;
  encodeCall(address: string, args: Json | null, auth: Json | null, context: Json | null): string;
  encodeFetch(address: string, params: Json | null): string;
  encodeOperation(id: string): string;
}

// --- transport seams -----------------------------------------------------------------

/// The subset of a `fetch` `Response` the shell reads.
export interface FetchResponse {
  readonly ok: boolean;
  readonly status: number;
  text(): Promise<string>;
}

/// A `fetch`-shaped POST function. `globalThis.fetch` satisfies it; a test injects a
/// mock. The shell only ever POSTs (GET is the EventSource stream). Auth rides the
/// request headers (the connection capability), so no ambient credential is required.
export type FetchLike = (
  url: string,
  init: { method: string; headers: Record<string, string>; body?: string },
) => Promise<FetchResponse>;

/// One dispatched SSE event as the shell consumes it: the frame JSON and the frontier
/// token (the SSE `id:`, surfaced natively as `lastEventId`).
export interface StreamEvent {
  readonly data: string;
  readonly lastEventId: string;
}

/// The `readyState` of an EventSource-like source, matching the web platform's values.
export const READY_CONNECTING = 0;
export const READY_OPEN = 1;
export const READY_CLOSED = 2;

/// The SSE event type that carries the stream-session announcement (the stream's first
/// event), kept distinct from the default `message` events that carry §12.2 wire frames
/// so the wasm core only ever sees frames.
export const STREAM_SESSION_EVENT = "liasse-session";

/// The EventSource surface the shell drives — a structural subset of the browser API
/// plus `close`, `readyState`, and the `liasse-session` announcement event, so a native
/// `EventSource` or a polyfill both satisfy it. `readyState` lets the shell tell a
/// self-reconnecting drop (`CONNECTING`) from a source that gave up (`CLOSED`) and must
/// be rebuilt (see `Stream`).
export interface EventSourceLike {
  readonly readyState: number;
  addEventListener(type: "message", handler: (event: StreamEvent) => void): void;
  addEventListener(type: "liasse-session", handler: (event: StreamEvent) => void): void;
  addEventListener(type: "error", handler: (event: unknown) => void): void;
  addEventListener(type: "open", handler: () => void): void;
  close(): void;
}

/// What the shell asks a transport to open the downstream SSE stream with — just the
/// endpoint. The stream is opened ANONYMOUSLY: it carries no capability in the URL, no
/// token, and no cookie is relied on. Opening it yields only a fresh, empty ephemeral
/// stream-session (announced on the first event); data flows only once an authenticated
/// POST binds a subscription to that session. There is therefore no presentable
/// credential to steal that would grant access to an existing stream.
export interface StreamRequest {
  readonly url: string;
}

/// Opens the downstream SSE stream. The default adapts a native `EventSource`; a
/// deployment can inject any transport that reconnects on drop or reports `CLOSED`.
export type EventSourceFactory = (request: StreamRequest) => EventSourceLike;

/// Schedule a delayed stream reconnect; returns a canceller. Injectable via
/// `ConnectOptions.schedule` to customize the reconnect backoff (or drive it
/// deterministically in a test). The default uses `setTimeout`.
export type Schedule = (run: () => void, delayMs: number) => () => void;

/// The observable state of the downstream connection. `reconnecting` covers both a
/// transport self-reconnecting after a drop and the shell's own backoff rebuild.
export type ConnectionState = "connecting" | "open" | "reconnecting" | "closed";

/// The authentication a `hello` opens the connection with (§11) — opaque to the shell.
export interface Hello {
  readonly auth?: Json;
  readonly context?: Json;
}

/// Options for opening a subscription (§12.2). All optional and engine-interpreted.
export interface SubscribeOptions {
  readonly params?: Json;
  readonly window?: Window;
  readonly auth?: Json;
  readonly context?: Json;
}

/// Options for a `call` (§10, §12.3). `operationId` seeds the idempotency capability;
/// when omitted the shell mints one with `crypto.randomUUID()`.
export interface CallOptions {
  readonly operationId?: string;
  readonly auth?: Json;
  readonly context?: Json;
}

/// Injectable transport/core overrides for `connect` — the defaults suit a browser.
export interface ConnectOptions {
  readonly core?: WireCore;
  readonly fetch?: FetchLike;
  readonly eventSource?: EventSourceFactory;
  /// Customize the stream's reconnect backoff schedule (default `setTimeout`).
  readonly schedule?: Schedule;
}
