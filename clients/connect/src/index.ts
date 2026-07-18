//! `@liasse/connect` — the untrusted browser client for the Liasse client-sync
//! connector (§12). A thin SSE + fetch transport shell over the `liasse-connect-wasm`
//! core: the wasm core owns all §12.2 wire semantics (frame decode, patch apply,
//! request encode); this package adds the EventSource lifecycle, fetch POSTs with
//! capability-header attachment, a store-like subscription surface, and reset →
//! re-subscribe. It holds no authority — every token is opaque data it echoes back.

export { connect, Session } from "./session.js";
export type { SessionErrorListener, ConnectionStateListener } from "./session.js";
export { Subscription } from "./subscription.js";
export type { ViewIntent, StateListener, ErrorListener } from "./subscription.js";
export { loadCore } from "./wasm.js";
export { defaultEventSourceFactory } from "./stream.js";
export type { FrameSink, StreamHooks } from "./stream.js";
export { READY_CONNECTING, READY_OPEN, READY_CLOSED, STREAM_SESSION_EVENT } from "./types.js";

export { ConnectError, TransportError, FaultError, ProtocolError, toConnectError } from "./errors.js";
export type { ConnectErrorKind } from "./errors.js";

export {
  asConnectionToken,
  asFrontierToken,
  asOperationId,
  asStreamSession,
  asSubId,
} from "./types.js";
export type {
  Anchor,
  Applied,
  CallOptions,
  CloseReason,
  ConnectionState,
  ConnectOptions,
  ConnectionToken,
  EventSourceFactory,
  EventSourceLike,
  Fault,
  FaultCode,
  FetchLike,
  FetchResponse,
  FrontierToken,
  Hello,
  Json,
  OperationHandleCore,
  OperationId,
  Outcome,
  ResetReason,
  Schedule,
  StreamEvent,
  StreamRequest,
  StreamSession,
  SubId,
  SubscribeOptions,
  ViewState,
  ViewStatus,
  WireClientCore,
  WireCore,
  WireRow,
  Window,
} from "./types.js";
