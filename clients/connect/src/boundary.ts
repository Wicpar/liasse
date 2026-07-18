//! Parse, don't validate: the one place `unknown` from the wire or the wasm core
//! becomes a typed value. Past here the rest of the shell works with `Applied`,
//! `Outcome`, and tokens — never with raw JSON it re-checks (AGENTS.md).
//!
//! The wasm core's results are Rust-guaranteed in shape, but the shell still narrows
//! them once so a `kind`/`status` mismatch surfaces as a `ProtocolError` here rather
//! than as an undefined access deep in the store.

import { ProtocolError } from "./errors.js";
import type {
  Applied,
  CloseReason,
  ConnectionToken,
  Fault,
  FaultCode,
  FrontierToken,
  Json,
  Outcome,
  ResetReason,
  SubId,
  WireRow,
} from "./types.js";
import { asConnectionToken, asFrontierToken, asSubId } from "./types.js";

/// A JSON object, the only `unknown` shape the wire returns for a structured reply.
type Obj = Record<string, unknown>;

function isObject(value: unknown): value is Obj {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function field(obj: Obj, name: string): unknown {
  return Object.prototype.hasOwnProperty.call(obj, name) ? obj[name] : undefined;
}

function stringField(obj: Obj, name: string, context: string): string {
  const value = field(obj, name);
  if (typeof value !== "string") {
    throw new ProtocolError(`${context}: expected string field \`${name}\``);
  }
  return value;
}

/// Narrow the wasm core's `applyFrame` result into a discriminated `Applied`.
export function parseApplied(raw: unknown): Applied {
  if (!isObject(raw)) {
    throw new ProtocolError("applied result is not an object");
  }
  const kind = field(raw, "kind");
  switch (kind) {
    case "init":
    case "patch":
      return {
        kind,
        sub: asSubId(stringField(raw, "sub", "applied")),
        frontier: asFrontierToken(stringField(raw, "frontier", "applied")),
        rows: parseRows(field(raw, "rows")),
      };
    case "scalar":
      return {
        kind: "scalar",
        sub: asSubId(stringField(raw, "sub", "applied")),
        frontier: asFrontierToken(stringField(raw, "frontier", "applied")),
        scalar: (field(raw, "scalar") ?? null) as Json,
      };
    case "close":
      return {
        kind: "close",
        sub: asSubId(stringField(raw, "sub", "applied")),
        closeReason: stringField(raw, "close_reason", "applied") as CloseReason,
      };
    case "frontier":
      return { kind: "frontier", frontier: asFrontierToken(stringField(raw, "frontier", "applied")) };
    case "reset":
      return { kind: "reset", resetReason: stringField(raw, "reset_reason", "applied") as ResetReason };
    case "fault":
      return { kind: "fault", fault: parseFault(field(raw, "fault")) };
    default:
      throw new ProtocolError(`applied result has unknown kind \`${String(kind)}\``);
  }
}

function parseFault(raw: unknown): Fault {
  if (!isObject(raw)) {
    throw new ProtocolError("fault is not an object");
  }
  return {
    code: stringField(raw, "code", "fault") as FaultCode,
    message: stringField(raw, "message", "fault"),
  };
}

/// Narrow the wasm core's `rows` result (an array of `{ id, value }`) into wire rows.
export function parseRows(raw: unknown): WireRow[] {
  if (!Array.isArray(raw)) {
    throw new ProtocolError("rows result is not an array");
  }
  return raw.map((entry, index) => {
    if (!isObject(entry)) {
      throw new ProtocolError(`row ${index} is not an object`);
    }
    return { id: stringField(entry, "id", "row"), value: (field(entry, "value") ?? null) as Json };
  });
}

/// The scalar value the core holds, or `null` for any non-scalar shape.
export function parseScalar(raw: unknown): Json | null {
  return (raw ?? null) as Json | null;
}

/// The frontier a `frontier`/`isClosed` read may return — tagged or `undefined`.
export function parseFrontier(raw: string | undefined): FrontierToken | undefined {
  return raw === undefined ? undefined : asFrontierToken(raw);
}

/// The close reason a read may return — tagged or `undefined`.
export function parseCloseReason(raw: string | undefined): CloseReason | undefined {
  return raw === undefined ? undefined : (raw as CloseReason);
}

/// The connection capability minted by a `hello` reply (`{ connection }`).
export function parseHelloConnection(raw: unknown): ConnectionToken {
  if (!isObject(raw)) {
    throw new ProtocolError("hello reply is not an object");
  }
  return asConnectionToken(stringField(raw, "connection", "hello reply"));
}

/// Confirm a `view` reply opened (`{ frontier }`) and surface its opening frontier.
export function parseOpened(raw: unknown): FrontierToken {
  if (!isObject(raw)) {
    throw new ProtocolError("view reply is not an object");
  }
  return asFrontierToken(stringField(raw, "frontier", "view reply"));
}

/// The manifest reply's exposed surfaces (`{ surfaces }`).
export function parseManifest(raw: unknown): Json {
  if (!isObject(raw)) {
    throw new ProtocolError("manifest reply is not an object");
  }
  return (field(raw, "surfaces") ?? null) as Json;
}

/// Narrow a request reply into a status-tagged `Outcome` (§8.9, §12.3).
export function parseOutcome(raw: unknown): Outcome {
  if (!isObject(raw)) {
    throw new ProtocolError("outcome is not an object");
  }
  const status = field(raw, "status");
  const response = field(raw, "response");
  switch (status) {
    case "committed":
      return {
        status: "committed",
        frontier: asFrontierToken(stringField(raw, "frontier", "outcome")),
        commit: asFrontierToken(stringField(raw, "commit", "outcome")),
        ...(response === undefined ? {} : { response: response as Json }),
      };
    case "unchanged":
      return {
        status: "unchanged",
        frontier: asFrontierToken(stringField(raw, "frontier", "outcome")),
        ...(response === undefined ? {} : { response: response as Json }),
      };
    case "rejected":
    case "denied":
      return {
        status,
        code: stringField(raw, "code", "outcome"),
        message: stringField(raw, "message", "outcome"),
      };
    case "failed":
      return { status: "failed", code: stringField(raw, "code", "outcome") as "absent-anchor" | "scalar-view" };
    case "unknown":
      return { status: "unknown" };
    default:
      throw new ProtocolError(`outcome has unknown status \`${String(status)}\``);
  }
}

/// A raw fetched value (§12.1) is carried verbatim — no wrapping envelope.
export function parseFetched(raw: unknown): Json {
  return raw as Json;
}
