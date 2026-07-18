//! The downstream SSE lifecycle: one EventSource per logical connection, carrying the
//! init/scalar/patch/close/frontier/reset/fault frames of every subscription — with
//! auto-reconnect and bad-network resilience.
//!
//! The SSE channel carries no auth token of its own. Auth lives on the POSTs (§12:
//! `hello` authenticates, `view`/`unsubscribe` open and close subscriptions). The
//! stream is bound to its connection by an AMBIENT credential — the HttpOnly cookie the
//! `hello` response sets — which the browser attaches automatically under
//! `withCredentials`. The default transport therefore places NO capability in the SSE
//! URL, and a native `EventSource` (which cannot set headers) still drives the stream.
//!
//! Anti-theft (this is the point of the cookie): a bearer token in the SSE URL would
//! leak through browser history, server access logs, and the `Referer` header — and
//! anyone who saw it could OPEN the same URL and STEAL the victim's stream, reading
//! their already-authorized frames. So the connection capability never goes in the URL.
//! Only a non-secret resume marker (the frontier token, useless without the credential)
//! may ride the URL, and only to resume a manual rebuild. A cross-origin deployment
//! that cannot use cookies must exchange the connection for a short-lived, single-use
//! STREAM TICKET in a custom factory — never place the connection capability itself in a
//! URL.
//!
//! Resilience: the SSE `id:` is the §12.2 frontier token, so resume is `Last-Event-ID`.
//! A native `EventSource` reconnects on a transient drop and replays it automatically
//! (readyState stays `CONNECTING`); the shell waits rather than open a second stream.
//! If the source gives up (readyState `CLOSED`), or an injected transport does not
//! self-reconnect, the shell rebuilds it with exponential backoff, carrying the last
//! observed frontier so a fresh source still resumes (§12.2). If the server cannot
//! replay, it sends `reset` and the session re-subscribes — the stream self-heals and
//! nothing throws on a bad network.

import type {
  ConnectionState,
  ConnectionToken,
  EventSourceFactory,
  EventSourceLike,
  StreamRequest,
} from "./types.js";
import { READY_CLOSED } from "./types.js";

/// A callback fed one raw downstream frame: its JSON `data` and its frontier `id`.
export type FrameSink = (data: string, frontier: string) => void;

/// What a caller observes on the stream: each frame, and each connection-state change.
export interface StreamHooks {
  readonly onFrame: FrameSink;
  readonly onState?: (state: ConnectionState) => void;
}

/// Schedule a delayed rebuild; returns a canceller. Injectable so a test drives the
/// backoff deterministically without real timers.
export type Schedule = (run: () => void, delayMs: number) => () => void;

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

/// Owns the single EventSource for a connection, routes its frames to a sink, and keeps
/// it alive across drops.
export class Stream {
  private readonly factory: EventSourceFactory;
  private readonly url: string;
  private readonly connection: ConnectionToken;
  private readonly schedule: Schedule;

  private source: EventSourceLike | undefined;
  private hooks: StreamHooks | undefined;
  private lastEventId = "";
  private state: ConnectionState = "connecting";
  private attempt = 0;
  private cancelReconnect: (() => void) | undefined;
  private stopped = false;

  constructor(
    factory: EventSourceFactory,
    url: string,
    connection: ConnectionToken,
    schedule: Schedule = defaultSchedule,
  ) {
    this.factory = factory;
    this.url = url;
    this.connection = connection;
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
    const request: StreamRequest = {
      url: this.url,
      connection: this.connection,
      ...(this.lastEventId === "" ? {} : { lastEventId: this.lastEventId }),
    };
    const source = this.factory(request);
    this.source = source;

    source.addEventListener("message", (event) => {
      if (source !== this.source) {
        return; // A frame from a stale source we already replaced.
      }
      this.markProgress();
      if (event.lastEventId !== "") {
        this.lastEventId = event.lastEventId;
      }
      this.hooks?.onFrame(event.data, event.lastEventId);
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

  /// A frame or an `open` proves the stream is live: clear any pending rebuild and
  /// reset the backoff so the next drop starts from the base delay again.
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

/// The default factory: adapt a native `EventSource`, bound to its connection by the
/// HttpOnly cookie the browser sends under `withCredentials`. The connection capability
/// is deliberately NOT in the URL (see the anti-theft note above); only a non-secret
/// resume marker is. Only invoked where a native `EventSource` exists (a browser, or
/// node with the global); a test or a non-browser host injects a transport instead, so
/// this is never on the test path.
export const defaultEventSourceFactory: EventSourceFactory = ({ url, lastEventId }) => {
  const ctor = (globalThis as { EventSource?: NativeEventSourceCtor }).EventSource;
  if (ctor === undefined) {
    throw new Error("no native EventSource is available; pass an EventSourceFactory via connect(..., { eventSource })");
  }
  const native = new ctor(resumeUrl(url, lastEventId), { withCredentials: true });
  return adaptNative(native);
};

/// A minimal view of the native `EventSource`, so this module needs no DOM lib: the
/// shell only reads `readyState`, adds listeners, and closes.
interface NativeEventSourceCtor {
  new (url: string, init?: { withCredentials?: boolean }): NativeEventSource;
}

interface NativeEventSource {
  readonly readyState: number;
  addEventListener(type: string, handler: (event: unknown) => void): void;
  close(): void;
}

/// Adapt a native `EventSource` (whose `message` events carry `data`/`lastEventId`) to
/// the shell's `EventSourceLike`.
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

/// Build the SSE URL. The connection is bound by the ambient HttpOnly cookie, so it is
/// NEVER put here — only a `lastEventId` (present on a manual rebuild) rides the URL, and
/// only because a fresh native `EventSource` cannot replay it from the platform's own
/// memory. The frontier token is not a credential: it cannot open a stream on its own.
function resumeUrl(url: string, lastEventId: string | undefined): string {
  if (lastEventId === undefined || lastEventId === "") {
    return url;
  }
  const separator = url.includes("?") ? "&" : "?";
  return `${url}${separator}last-event-id=${encodeURIComponent(lastEventId)}`;
}
