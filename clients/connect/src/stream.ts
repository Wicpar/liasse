//! The downstream SSE lifecycle: one anonymous EventSource per connection, carrying the
//! init/scalar/patch/close/frontier/reset/fault frames of every subscription — with
//! auto-reconnect and bad-network resilience.
//!
//! Ephemeral, unstealable sessions. The stream is opened ANONYMOUSLY — no token in the
//! URL, no cookie relied on. On connect the server mints a fresh, single-socket
//! stream-session and announces its id on the stream's first event (a `liasse-session`
//! event, kept separate from the `message` events that carry §12.2 wire frames). The
//! shell reports that id to the session, which then binds its subscriptions to it with
//! AUTHENTICATED POSTs. So there is no presentable credential that grants access to a
//! stream: opening the URL only yields a new empty session, and data flows only after
//! an authorized bind — nothing in a URL, cookie, or log can steal an existing stream.
//!
//! Resilience: a native `EventSource` reconnects on a transient drop (readyState stays
//! `CONNECTING`); the shell waits rather than open a second stream. If the source gives
//! up (`CLOSED`), or an injected transport does not self-reconnect, the shell rebuilds
//! it with exponential backoff. Every (re)connect is a NEW ephemeral session announced
//! on the new socket's first event; the session re-binds its subscriptions to it (§12.2
//! `init` re-establishes the rows). Nothing throws on a bad network.

import type {
  ConnectionState,
  EventSourceFactory,
  EventSourceLike,
  Schedule,
  StreamRequest,
  StreamSession,
} from "./types.js";
import { READY_CLOSED, STREAM_SESSION_EVENT, asStreamSession } from "./types.js";

/// A callback fed one raw downstream frame: its JSON `data` and its frontier `id`.
export type FrameSink = (data: string, frontier: string) => void;

/// What a caller observes on the stream: each frame, each announced stream-session (once
/// per (re)connect), and each connection-state change.
export interface StreamHooks {
  readonly onFrame: FrameSink;
  readonly onSession: (session: StreamSession) => void;
  readonly onState?: (state: ConnectionState) => void;
}

const defaultSchedule: Schedule = (run, delayMs) => {
  const handle = setTimeout(run, delayMs);
  return () => clearTimeout(handle);
};

/// Exponential backoff with a cap and jitter (base 1s, cap 30s). Jitter spreads
/// reconnect storms; the ranges stay strictly increasing so backoff visibly grows.
function backoffDelay(attempt: number): number {
  const base = 1000;
  const cap = 30_000;
  const exponential = Math.min(cap, base * 2 ** attempt);
  return exponential + Math.random() * base;
}

/// Owns the single anonymous EventSource for a connection, routes its frames and its
/// session announcement to the session, and keeps it alive across drops.
export class Stream {
  private readonly factory: EventSourceFactory;
  private readonly url: string;
  private readonly schedule: Schedule;

  private source: EventSourceLike | undefined;
  private hooks: StreamHooks | undefined;
  private state: ConnectionState = "connecting";
  private attempt = 0;
  private cancelReconnect: (() => void) | undefined;
  private stopped = false;

  constructor(factory: EventSourceFactory, url: string, schedule: Schedule = defaultSchedule) {
    this.factory = factory;
    this.url = url;
    this.schedule = schedule;
  }

  /// Open the stream and start delivering frames. Idempotent.
  open(hooks: StreamHooks): void {
    if (this.hooks !== undefined) {
      return;
    }
    this.hooks = hooks;
    this.connect();
  }

  /// The current connection state.
  get connectionState(): ConnectionState {
    return this.state;
  }

  /// Close the stream permanently: cancel any pending reconnect and never reopen.
  close(): void {
    this.stopped = true;
    this.cancelReconnect?.();
    this.cancelReconnect = undefined;
    this.source?.close();
    this.source = undefined;
    this.setState("closed");
  }

  private connect(): void {
    if (this.stopped) {
      return;
    }
    this.setState(this.attempt === 0 ? "connecting" : "reconnecting");
    const request: StreamRequest = { url: this.url };
    const source = this.factory(request);
    this.source = source;

    source.addEventListener("message", (event) => {
      if (source !== this.source) {
        return; // A frame from a stale source we already replaced.
      }
      this.markProgress();
      this.hooks?.onFrame(event.data, event.lastEventId);
    });
    source.addEventListener(STREAM_SESSION_EVENT, (event) => {
      if (source !== this.source) {
        return;
      }
      this.markProgress();
      const session = parseSession(event.data);
      if (session !== undefined) {
        this.hooks?.onSession(session);
      }
    });
    source.addEventListener("open", () => {
      if (source === this.source) {
        this.markProgress();
      }
    });
    source.addEventListener("error", () => {
      if (source === this.source) {
        this.onError(source);
      }
    });
  }

  /// A frame, a session announcement, or an `open` proves the stream is live: clear any
  /// pending rebuild and reset the backoff so the next drop starts from the base delay.
  private markProgress(): void {
    this.attempt = 0;
    this.cancelReconnect?.();
    this.cancelReconnect = undefined;
    this.setState("open");
  }

  private onError(source: EventSourceLike): void {
    if (this.stopped) {
      return;
    }
    this.setState("reconnecting");
    // A `CLOSED` source will not come back on its own (a native EventSource that hit a
    // fatal error, or an injected transport that does not self-reconnect) → rebuild it
    // with backoff. A still-`CONNECTING` source is the transport reconnecting itself;
    // opening a second stream would double it, so we wait for its own recovery.
    if (source.readyState === READY_CLOSED) {
      this.scheduleReconnect();
    }
  }

  private scheduleReconnect(): void {
    if (this.cancelReconnect !== undefined) {
      return;
    }
    const delay = backoffDelay(this.attempt);
    this.attempt += 1;
    this.cancelReconnect = this.schedule(() => {
      this.cancelReconnect = undefined;
      this.source?.close();
      this.source = undefined;
      this.connect();
    }, delay);
  }

  private setState(state: ConnectionState): void {
    if (this.state === state) {
      return;
    }
    this.state = state;
    this.hooks?.onState?.(state);
  }
}

/// Parse a `liasse-session` announcement (`{ "stream": "<id>" }`). Total: malformed data
/// yields `undefined` rather than throwing, and the stream simply awaits the next one.
function parseSession(data: string): StreamSession | undefined {
  try {
    const parsed = JSON.parse(data) as unknown;
    if (typeof parsed === "object" && parsed !== null) {
      const stream = (parsed as { stream?: unknown }).stream;
      if (typeof stream === "string" && stream.length > 0) {
        return asStreamSession(stream);
      }
    }
  } catch {
    // fall through to undefined
  }
  return undefined;
}

/// The default factory: adapt a native `EventSource`, opened ANONYMOUSLY. There is no
/// capability in the URL and no cookie is relied on — the stream's identity comes from
/// the server's `liasse-session` announcement, and its frames only from authenticated
/// bind POSTs. Only invoked where a native `EventSource` exists (a browser, or node with
/// the global); a test or a non-browser host injects a transport instead.
export const defaultEventSourceFactory: EventSourceFactory = ({ url }) => {
  const ctor = (globalThis as { EventSource?: NativeEventSourceCtor }).EventSource;
  if (ctor === undefined) {
    throw new Error("no native EventSource is available; pass an EventSourceFactory via connect(..., { eventSource })");
  }
  return adaptNative(new ctor(url));
};

/// A minimal view of the native `EventSource`, so this module needs no DOM lib: the
/// shell only reads `readyState`, adds listeners, and closes.
interface NativeEventSourceCtor {
  new (url: string): NativeEventSource;
}

interface NativeEventSource {
  readonly readyState: number;
  addEventListener(type: string, handler: (event: unknown) => void): void;
  close(): void;
}

/// Adapt a native `EventSource` (whose `message`/`liasse-session` events carry
/// `data`/`lastEventId`) to the shell's `EventSourceLike`.
function adaptNative(native: NativeEventSource): EventSourceLike {
  return {
    get readyState(): number {
      return native.readyState;
    },
    addEventListener(type: string, handler: (event: never) => void): void {
      native.addEventListener(type, handler as (event: unknown) => void);
    },
    close(): void {
      native.close();
    },
  } as EventSourceLike;
}
