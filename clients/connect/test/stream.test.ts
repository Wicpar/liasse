//! The SSE stream's resilience and anti-theft, unit-tested with a fake source and a
//! captured scheduler (no real timers or network). Proves the shell: opens the stream
//! ANONYMOUSLY (no token, no credential in the URL — nothing to steal), reports the
//! server-announced ephemeral session, folds frames; waits out a self-reconnecting drop
//! without opening a second stream; rebuilds a source that gave up, with backoff that
//! grows on consecutive failures and RESETS after recovery, announcing a fresh session
//! each time; and never reconnects once closed. Expected behaviour is deduced from the
//! SSE/§12.2 model, not from the shell's own output.

import assert from "node:assert/strict";
import { test } from "node:test";

import { Stream, defaultEventSourceFactory } from "../src/stream.js";
import { READY_CLOSED, READY_CONNECTING, STREAM_SESSION_EVENT } from "../src/index.js";
import type { ConnectionState, EventSourceLike, Schedule, StreamEvent, StreamRequest } from "../src/index.js";

/// A fully controllable EventSource: the test drives its readyState and events.
class FakeSource implements EventSourceLike {
  readonly request: StreamRequest;
  readyState = READY_CONNECTING;
  private readonly message: ((event: StreamEvent) => void)[] = [];
  private readonly session: ((event: StreamEvent) => void)[] = [];
  private readonly opened: (() => void)[] = [];
  private readonly errored: (() => void)[] = [];

  constructor(request: StreamRequest) {
    this.request = request;
  }

  addEventListener(type: "message", handler: (event: StreamEvent) => void): void;
  addEventListener(type: "liasse-session", handler: (event: StreamEvent) => void): void;
  addEventListener(type: "error", handler: (event: unknown) => void): void;
  addEventListener(type: "open", handler: () => void): void;
  addEventListener(type: string, handler: (event: never) => void): void {
    if (type === "message") this.message.push(handler as unknown as (event: StreamEvent) => void);
    else if (type === STREAM_SESSION_EVENT) this.session.push(handler as unknown as (event: StreamEvent) => void);
    else if (type === "open") this.opened.push(handler as unknown as () => void);
    else if (type === "error") this.errored.push(handler as unknown as () => void);
  }

  close(): void {
    this.readyState = READY_CLOSED;
  }

  frame(data: string, id: string): void {
    this.readyState = 1;
    for (const handler of this.message) handler({ data, lastEventId: id });
  }

  announce(stream: string): void {
    this.readyState = 1;
    const data = JSON.stringify({ stream });
    for (const handler of this.session) handler({ data, lastEventId: "" });
  }

  open(): void {
    this.readyState = 1;
    for (const handler of this.opened) handler();
  }

  drop(readyState: number): void {
    this.readyState = readyState;
    for (const handler of this.errored) handler();
  }
}

/// A scheduler that captures the pending rebuild so the test fires it deliberately.
function captureScheduler(): { schedule: Schedule; run(): void; delay(): number; pending(): boolean } {
  let job: { run: () => void; delayMs: number } | undefined;
  return {
    schedule: (run, delayMs) => {
      job = { run, delayMs };
      return () => {
        job = undefined;
      };
    },
    run() {
      const current = job;
      assert.ok(current, "a reconnect was scheduled");
      job = undefined;
      current.run();
    },
    delay() {
      assert.ok(job, "a reconnect was scheduled");
      return job.delayMs;
    },
    pending() {
      return job !== undefined;
    },
  };
}

function harness(): {
  stream: Stream;
  sources: FakeSource[];
  frames: [string, string][];
  sessions: string[];
  states: ConnectionState[];
  scheduler: ReturnType<typeof captureScheduler>;
} {
  const sources: FakeSource[] = [];
  const frames: [string, string][] = [];
  const sessions: string[] = [];
  const states: ConnectionState[] = [];
  const scheduler = captureScheduler();
  const stream = new Stream(
    (request) => {
      const source = new FakeSource(request);
      sources.push(source);
      return source;
    },
    "http://liasse.test",
    scheduler.schedule,
  );
  stream.open({
    onFrame: (data, id) => frames.push([data, id]),
    onSession: (session) => sessions.push(session),
    onState: (state) => states.push(state),
  });
  return { stream, sources, frames, sessions, states, scheduler };
}

test("the stream opens anonymously, reports the announced session, and folds frames", () => {
  const { sources, frames, sessions } = harness();

  assert.equal(sources.length, 1);
  const first = sources[0];
  assert.ok(first);
  // Anonymous open: the request carries only the URL — no token, nothing to steal.
  assert.deepEqual(Object.keys(first.request), ["url"]);

  first.open();
  first.announce("strm-1");
  assert.deepEqual(sessions, ["strm-1"], "the server-announced session is reported");

  // A wire frame arrives on the default `message` channel, distinct from the session.
  first.frame('{"type":"frontier"}', "f1");
  assert.deepEqual(frames.at(-1), ['{"type":"frontier"}', "f1"]);
});

test("a source that gave up is rebuilt with backoff and announces a fresh session", () => {
  const { stream, sources, sessions, scheduler } = harness();
  const first = sources[0];
  assert.ok(first);
  first.open();
  first.announce("strm-1");
  first.frame('{"type":"frontier"}', "f1");

  // The source reports CLOSED — it will not recover on its own → schedule a rebuild.
  first.drop(READY_CLOSED);
  assert.equal(stream.connectionState, "reconnecting");
  const firstDelay = scheduler.delay();
  assert.ok(firstDelay >= 1000, "backoff starts at the base delay");

  scheduler.run();
  assert.equal(sources.length, 2, "a fresh source was built");
  const second = sources[1];
  assert.ok(second);
  assert.deepEqual(Object.keys(second.request), ["url"], "the rebuilt source is still anonymous");

  // Every (re)connect is a NEW ephemeral session; the shell reports the new id so the
  // session can re-bind its subscriptions to it.
  second.announce("strm-2");
  assert.deepEqual(sessions, ["strm-1", "strm-2"]);
});

test("consecutive failures grow the backoff and a recovery resets it", () => {
  const { stream, sources, scheduler } = harness();
  const first = sources[0];
  assert.ok(first);
  first.open();

  first.drop(READY_CLOSED);
  const delay0 = scheduler.delay();
  scheduler.run();

  const second = sources[1];
  assert.ok(second);
  second.drop(READY_CLOSED); // fails again before any recovery
  const delay1 = scheduler.delay();
  assert.ok(delay1 > delay0, "backoff grows on consecutive failures");
  scheduler.run();

  const third = sources[2];
  assert.ok(third);
  third.open(); // recovery resets the backoff
  third.drop(READY_CLOSED);
  const delay2 = scheduler.delay();
  assert.ok(delay2 < delay1, "backoff resets after a successful open");
  assert.equal(stream.connectionState, "reconnecting");
});

test("a self-reconnecting drop is not doubled by a second stream", () => {
  const { stream, sources, scheduler } = harness();
  const first = sources[0];
  assert.ok(first);
  first.open();

  // Still CONNECTING: the transport is reconnecting itself → the shell must wait.
  first.drop(READY_CONNECTING);
  assert.equal(stream.connectionState, "reconnecting");
  assert.equal(scheduler.pending(), false, "no manual rebuild while the source self-reconnects");
  assert.equal(sources.length, 1, "no second stream opened");

  // The transport recovers on its own.
  first.frame('{"type":"frontier"}', "f2");
  assert.equal(stream.connectionState, "open");
});

test("close cancels any pending reconnect and never reopens", () => {
  const { stream, sources, scheduler } = harness();
  const first = sources[0];
  assert.ok(first);
  first.open();
  first.drop(READY_CLOSED);
  assert.equal(scheduler.pending(), true);

  stream.close();
  assert.equal(stream.connectionState, "closed");
  assert.equal(scheduler.pending(), false, "the pending reconnect was cancelled");

  // A late error from the dead source must not resurrect the stream.
  first.drop(READY_CLOSED);
  assert.equal(scheduler.pending(), false);
  assert.equal(sources.length, 1);
});

test("the default transport opens the stream anonymously — no token or credential", () => {
  const seen: { url: string; init: unknown }[] = [];
  class FakeNative {
    readyState = READY_CONNECTING;
    constructor(url: string, init?: unknown) {
      seen.push({ url, init });
    }
    addEventListener(): void {}
    close(): void {}
  }

  const holder = globalThis as { EventSource?: unknown };
  const saved = holder.EventSource;
  holder.EventSource = FakeNative;
  try {
    defaultEventSourceFactory({ url: "https://app.example/liasse" });
  } finally {
    holder.EventSource = saved;
  }

  assert.equal(seen.length, 1);
  const opened = seen[0];
  assert.ok(opened);
  // Anonymous open: the exact base URL — no query, no token, nothing presentable that
  // anyone could use to open (steal) this stream. The stream's identity comes from the
  // server's announcement, and its data only from authenticated bind POSTs.
  assert.equal(opened.url, "https://app.example/liasse");
  assert.ok(!opened.url.includes("?"), `the anonymous stream URL carries no query: ${opened.url}`);
  // And no credential init (no `withCredentials`) — `new EventSource(url)`, one argument.
  assert.equal(opened.init, undefined);
});
