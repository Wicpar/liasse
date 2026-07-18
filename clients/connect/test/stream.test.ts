//! The SSE stream's resilience, unit-tested with a fake source and a captured
//! scheduler (no real timers or network). Proves the shell: opens the anonymous
//! channel with the connection HANDLE; folds frames; waits out a self-reconnecting
//! drop without opening a second stream; rebuilds a source that gave up, with growing
//! backoff that RESUMES from the last frontier and RESETS after recovery; and never
//! reconnects once closed. Expected behaviour is deduced from the SSE/§12.2 model, not
//! from the shell's own output.

import assert from "node:assert/strict";
import { test } from "node:test";

import { Stream } from "../src/stream.js";
import type { Schedule } from "../src/stream.js";
import { asConnectionToken, READY_CLOSED, READY_CONNECTING } from "../src/index.js";
import type { ConnectionState, EventSourceLike, StreamEvent, StreamRequest } from "../src/index.js";

/// A fully controllable EventSource: the test drives its readyState and events.
class FakeSource implements EventSourceLike {
  readonly request: StreamRequest;
  readyState = READY_CONNECTING;
  private readonly message: ((event: StreamEvent) => void)[] = [];
  private readonly opened: (() => void)[] = [];
  private readonly errored: (() => void)[] = [];

  constructor(request: StreamRequest) {
    this.request = request;
  }

  addEventListener(type: "message", handler: (event: StreamEvent) => void): void;
  addEventListener(type: "error", handler: (event: unknown) => void): void;
  addEventListener(type: "open", handler: () => void): void;
  addEventListener(type: string, handler: (event: never) => void): void {
    if (type === "message") this.message.push(handler as unknown as (event: StreamEvent) => void);
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
  states: ConnectionState[];
  scheduler: ReturnType<typeof captureScheduler>;
} {
  const sources: FakeSource[] = [];
  const frames: [string, string][] = [];
  const states: ConnectionState[] = [];
  const scheduler = captureScheduler();
  const stream = new Stream(
    (request) => {
      const source = new FakeSource(request);
      sources.push(source);
      return source;
    },
    "http://liasse.test",
    asConnectionToken("conn-1"),
    scheduler.schedule,
  );
  stream.open({ onFrame: (data, id) => frames.push([data, id]), onState: (state) => states.push(state) });
  return { stream, sources, frames, states, scheduler };
}

test("the stream opens the anonymous channel with the connection handle and folds frames", () => {
  const { sources, frames } = harness();

  assert.equal(sources.length, 1);
  const first = sources[0];
  assert.ok(first);
  assert.equal(first.request.connection, "conn-1");
  assert.equal(first.request.lastEventId, undefined, "no resume token on the first connect");

  first.open();
  first.frame('{"type":"frontier"}', "f1");
  assert.deepEqual(frames.at(-1), ['{"type":"frontier"}', "f1"]);
});

test("a source that gave up is rebuilt with backoff, resuming from the last frontier", () => {
  const { stream, sources, scheduler } = harness();
  const first = sources[0];
  assert.ok(first);
  first.open();
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
  assert.equal(second.request.lastEventId, "f1", "the rebuild resumes from Last-Event-ID (§12.2)");
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
