//! Test harness: a mock server that plays the ephemeral-stream-session transport model,
//! and a controllable EventSource so a test can push exact §12.2 downstream frames at
//! the REAL wasm core through the shell.
//!
//! The mock reconstructs no wire semantics — it announces a fresh stream-session on each
//! stream connect (the first `liasse-session` event), answers `hello`/`view`/`call`/…
//! with the canned reply shapes the server writes, records the headers the shell
//! attached (including `liasse-stream` binding a subscription to the socket), and lets a
//! test emit downstream frames whose applied result is deduced from §12.2 by hand.

import { createRequire } from "node:module";

import { READY_CLOSED, READY_CONNECTING, READY_OPEN, STREAM_SESSION_EVENT } from "../src/index.js";
import type { EventSourceFactory, EventSourceLike, FetchLike, StreamEvent, StreamRequest, WireCore } from "../src/index.js";

/// Load the REAL wasm client core (the `--target nodejs` build) so tests drive the
/// same §12.2 logic the browser runs, not a hand-rolled fake.
export function loadNodeCore(): WireCore {
  const require = createRequire(import.meta.url);
  return require("../../wasm/node/liasse_connect_wasm.js") as unknown as WireCore;
}

/// One canned HTTP reply: a status and the exact body text the server would write.
interface Reply {
  readonly ok: boolean;
  readonly status: number;
  readonly text: string;
}

/// A recorded upstream request: the attached headers and the parsed body.
export interface Recorded {
  readonly headers: Record<string, string>;
  readonly body: Record<string, unknown>;
}

/// A controllable stand-in for a native `EventSource`: the test pushes frames into it
/// and drives its `readyState`/open/error to exercise reconnect handling.
export class MockEventSource implements EventSourceLike {
  readonly request: StreamRequest;
  readyState = READY_CONNECTING;
  closed = false;
  private readonly messageHandlers: ((event: StreamEvent) => void)[] = [];
  private readonly sessionHandlers: ((event: StreamEvent) => void)[] = [];
  private readonly openHandlers: (() => void)[] = [];
  private readonly errorHandlers: ((event: unknown) => void)[] = [];

  constructor(request: StreamRequest) {
    this.request = request;
  }

  addEventListener(type: "message", handler: (event: StreamEvent) => void): void;
  addEventListener(type: "liasse-session", handler: (event: StreamEvent) => void): void;
  addEventListener(type: "error", handler: (event: unknown) => void): void;
  addEventListener(type: "open", handler: () => void): void;
  addEventListener(type: string, handler: (event: never) => void): void {
    if (type === "message") {
      this.messageHandlers.push(handler as unknown as (event: StreamEvent) => void);
    } else if (type === STREAM_SESSION_EVENT) {
      this.sessionHandlers.push(handler as unknown as (event: StreamEvent) => void);
    } else if (type === "open") {
      this.openHandlers.push(handler as unknown as () => void);
    } else if (type === "error") {
      this.errorHandlers.push(handler as unknown as (event: unknown) => void);
    }
  }

  close(): void {
    this.closed = true;
    this.readyState = READY_CLOSED;
  }

  /// Announce the ephemeral stream-session (the stream's first `liasse-session` event).
  emitSession(stream: string): void {
    this.readyState = READY_OPEN;
    const data = JSON.stringify({ stream });
    for (const handler of this.sessionHandlers) {
      handler({ data, lastEventId: "" });
    }
  }

  /// Emit one dispatched SSE event (a downstream frame `data` at frontier `id`).
  push(data: string, id: string): void {
    this.readyState = READY_OPEN;
    for (const handler of this.messageHandlers) {
      handler({ data, lastEventId: id });
    }
  }

  /// Signal the stream opened.
  emitOpen(): void {
    this.readyState = READY_OPEN;
    for (const handler of this.openHandlers) {
      handler();
    }
  }

  /// Signal a drop, leaving the source in `readyState` (CONNECTING = self-reconnecting,
  /// CLOSED = gave up).
  emitError(readyState: number): void {
    this.readyState = readyState;
    for (const handler of this.errorHandlers) {
      handler(undefined);
    }
  }
}

/// A mock Liasse server over the connect transport contract.
export class MockServer {
  connection = "conn-1";
  callOutcome: unknown = { status: "committed", frontier: "f1", commit: "f1", response: { ok: true } };
  fetchValue: unknown = { title: "snapshot" };
  operationOutcome: unknown = { status: "unknown" };
  manifestBody: unknown = { surfaces: ["public.tasks"] };

  readonly requests: Recorded[] = [];
  readonly streams: MockEventSource[] = [];
  /// The stream-session ids announced so far, in connect order.
  readonly announced: string[] = [];
  autoAnnounce = true;
  private readonly viewCounts = new Map<string, number>();

  /// The `FetchLike` the shell POSTs to.
  readonly fetch: FetchLike = async (_url, init) => {
    const body = JSON.parse(init.body ?? "{}") as Record<string, unknown>;
    this.requests.push({ headers: init.headers, body });
    const reply = this.route(body);
    return { ok: reply.ok, status: reply.status, text: async () => reply.text };
  };

  /// The `EventSourceFactory` the shell opens the stream with. Each connect mints a
  /// fresh ephemeral stream-session and announces it on the socket's first event (on a
  /// microtask, after the shell has attached its listeners).
  readonly eventSource: EventSourceFactory = (request) => {
    const source = new MockEventSource(request);
    this.streams.push(source);
    if (this.autoAnnounce) {
      const stream = `strm-${this.streams.length}`;
      this.announced.push(stream);
      queueMicrotask(() => source.emitSession(stream));
    }
    return source;
  };

  /// The most recently announced stream-session id.
  get lastStream(): string {
    const last = this.announced.at(-1);
    if (last === undefined) {
      throw new Error("no stream-session announced");
    }
    return last;
  }

  /// The single open stream (fails loudly if none is open yet).
  get stream(): MockEventSource {
    const last = this.streams.at(-1);
    if (last === undefined) {
      throw new Error("no stream opened");
    }
    return last;
  }

  /// How many `view` requests the shell POSTed for a given subscription.
  viewCount(sub: string): number {
    return this.viewCounts.get(sub) ?? 0;
  }

  private route(body: Record<string, unknown>): Reply {
    const ok = (text: string): Reply => ({ ok: true, status: 200, text });
    switch (body["type"]) {
      case "hello":
        return ok(JSON.stringify({ connection: this.connection }));
      case "view": {
        const sub = String(body["sub"]);
        this.viewCounts.set(sub, this.viewCount(sub) + 1);
        return ok(JSON.stringify({ frontier: "f0" }));
      }
      case "unsubscribe":
        return ok("{}");
      case "call":
        return ok(JSON.stringify(this.callOutcome));
      case "fetch":
        return ok(JSON.stringify(this.fetchValue));
      case "operation":
        return ok(JSON.stringify(this.operationOutcome));
      case "manifest":
        return ok(JSON.stringify(this.manifestBody));
      default:
        return {
          ok: false,
          status: 400,
          text: JSON.stringify({ type: "fault", code: "malformed", message: "frame did not parse" }),
        };
    }
  }
}

/// Yield to the microtask/timer queue so pending POSTs settle before assertions.
export function tick(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

// --- hand-written §12.2 downstream frames (the server's wire form) -------------------

export function initFrame(sub: string, rows: { id: string; value: unknown }[]): string {
  return JSON.stringify({ type: "init", sub, rows });
}

export function patchFrame(sub: string, ops: unknown[]): string {
  return JSON.stringify({ type: "patch", sub, ops });
}

export function scalarFrame(sub: string, value: unknown): string {
  return JSON.stringify({ type: "scalar", sub, value });
}

export function closeFrame(sub: string, reason: string): string {
  return JSON.stringify({ type: "close", sub, reason });
}

export function resetFrame(reason: string): string {
  return JSON.stringify({ type: "reset", reason });
}
