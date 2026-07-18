//! The upstream request layer: fetch POSTs with the capability headers attached.
//!
//! Every request body is produced by the wasm core (the canonical wire codec); this
//! layer only chooses the URL, attaches the `Liasse-Connection` capability (auth), the
//! `Liasse-Stream` ephemeral stream-session (to bind/route downstream frames), and, on a
//! `call`, the `Liasse-Operation-Id` header (§12.3), then parses the reply. A non-2xx
//! response carries a downstream `fault` body, which becomes a typed error — never an
//! unhandled rejection the caller cannot inspect.

import { FaultError, TransportError } from "./errors.js";
import type { ConnectionToken, FaultCode, FetchLike, OperationId, StreamSession } from "./types.js";

/// Extra per-request metadata carried as headers (not in the body).
interface PostMeta {
  readonly operationId?: OperationId;
}

/// Posts request bodies to the one connection endpoint and parses their replies.
export class HttpTransport {
  private readonly baseUrl: string;
  private readonly fetch: FetchLike;
  private connection: ConnectionToken | undefined;
  private stream: StreamSession | undefined;

  constructor(baseUrl: string, fetch: FetchLike) {
    this.baseUrl = baseUrl;
    this.fetch = fetch;
  }

  /// Record the connection capability minted by `hello`; attached to later requests.
  setConnection(connection: ConnectionToken): void {
    this.connection = connection;
  }

  /// Record the current ephemeral stream-session (announced on the stream's first
  /// event); attached to later requests so the server binds/routes their downstream
  /// frames onto this socket. Updated on every (re)connect.
  setStream(stream: StreamSession): void {
    this.stream = stream;
  }

  /// The connection capability, once opened.
  get connectionToken(): ConnectionToken | undefined {
    return this.connection;
  }

  /// POST a wire body and parse its JSON reply. Throws a `FaultError` for a non-2xx
  /// (fault) response and a `TransportError` for a network or decode failure.
  async post(body: string, meta: PostMeta = {}): Promise<unknown> {
    const headers: Record<string, string> = { "content-type": "application/json" };
    if (this.connection !== undefined) {
      headers["liasse-connection"] = this.connection;
    }
    if (this.stream !== undefined) {
      headers["liasse-stream"] = this.stream;
    }
    if (meta.operationId !== undefined) {
      headers["liasse-operation-id"] = meta.operationId;
    }

    let response;
    try {
      response = await this.fetch(this.baseUrl, { method: "POST", headers, body });
    } catch (cause) {
      throw new TransportError(`request failed: ${errorText(cause)}`);
    }

    const text = await response.text();
    const parsed = parseJson(text);
    if (!response.ok) {
      throw faultFrom(parsed, response.status);
    }
    return parsed;
  }
}

/// Parse a reply body, tolerating an empty body (some replies are bare `{}`).
function parseJson(text: string): unknown {
  if (text.length === 0) {
    return {};
  }
  try {
    return JSON.parse(text) as unknown;
  } catch (cause) {
    throw new TransportError(`reply was not JSON: ${errorText(cause)}`);
  }
}

/// A non-2xx reply is a downstream `fault` frame `{ code, message }`; recover it, or
/// fall back to a status-only transport error.
function faultFrom(parsed: unknown, status: number): FaultError | TransportError {
  if (typeof parsed === "object" && parsed !== null) {
    const record = parsed as Record<string, unknown>;
    const code = record["code"];
    const message = record["message"];
    if (typeof code === "string" && typeof message === "string") {
      return new FaultError(code as FaultCode, message);
    }
  }
  return new TransportError(`request failed with status ${status}`, status);
}

function errorText(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}
