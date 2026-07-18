//! The shell's error taxonomy. A fault is always a HANDLED error, never an uncaught
//! throw that tears the shell down (AGENTS.md's no-panic discipline, in TypeScript):
//! the wasm core rejects a hostile frame by throwing a JS error, and the shell turns
//! that into one of these and routes it to a listener.

import type { FaultCode } from "./types.js";

/// What went wrong at the transport/wire boundary.
export type ConnectErrorKind =
  /// The HTTP request itself failed or returned a non-2xx status.
  | "transport"
  /// The server (or the wire) reported a `fault` frame.
  | "fault"
  /// A reply or frame did not match the wire schema the shell expects.
  | "protocol";

/// The base class for every error the shell surfaces. It is always caught internally
/// on the stream path and delivered to a handler — it never escapes a frame callback.
export class ConnectError extends Error {
  readonly kind: ConnectErrorKind;

  constructor(kind: ConnectErrorKind, message: string) {
    super(message);
    this.name = "ConnectError";
    this.kind = kind;
  }
}

/// An HTTP request failed or the server answered with a non-2xx status.
export class TransportError extends ConnectError {
  /// The HTTP status, when the request completed with one.
  readonly status: number | undefined;

  constructor(message: string, status?: number) {
    super("transport", message);
    this.name = "TransportError";
    this.status = status;
  }
}

/// The server reported a transport fault (a downstream `fault`): a forged capability,
/// a malformed or oversized frame, or an internal error. Carries no model state.
export class FaultError extends ConnectError {
  /// The stable fault class.
  readonly code: FaultCode;

  constructor(code: FaultCode, message: string) {
    super("fault", message);
    this.name = "FaultError";
    this.code = code;
  }
}

/// A reply or frame did not fit the wire schema — including a hostile downstream frame
/// the wasm core refused (a malformed frame, or a patch for an unopened subscription).
export class ProtocolError extends ConnectError {
  constructor(message: string) {
    super("protocol", message);
    this.name = "ProtocolError";
  }
}

/// Normalize anything thrown (a JS `Error`, a wasm error string, or an arbitrary
/// value) into a `ConnectError`, so a `catch` always yields a typed, handled error.
export function toConnectError(thrown: unknown, context: string): ConnectError {
  if (thrown instanceof ConnectError) {
    return thrown;
  }
  const detail =
    thrown instanceof Error
      ? thrown.message
      : typeof thrown === "string"
        ? thrown
        : JSON.stringify(thrown);
  return new ProtocolError(`${context}: ${detail}`);
}
