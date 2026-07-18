//! Integration: the @liasse/connect shell wired to the REAL wasm core (loaded under
//! node), with fetch + EventSource mocked. It proves the shell folds §12.2 frames
//! through the wasm replica into its store, attaches the capability headers, reopens
//! on `reset`, surfaces `close`, and turns a hostile frame into a HANDLED error rather
//! than an uncaught throw.
//!
//! Every expected state is deduced from §12.2 by hand (init sets the rows; an insert
//! adds at the given current-result index; close terminates), never from the shell's
//! own output.

import assert from "node:assert/strict";
import { test } from "node:test";

import { connect, FaultError, READY_CLOSED } from "../src/index.js";
import type { ConnectError, Session, ViewState } from "../src/index.js";
import {
  closeFrame,
  initFrame,
  loadNodeCore,
  MockServer,
  patchFrame,
  resetFrame,
  scalarFrame,
  tick,
} from "./support.js";

const core = loadNodeCore();

/// Open a session against a fresh mock server.
async function openSession(): Promise<{ server: MockServer; session: Session }> {
  const server = new MockServer();
  const session = await connect("http://liasse.test", {}, {
    core,
    fetch: server.fetch,
    eventSource: server.eventSource,
  });
  return { server, session };
}

test("connect opens a connection and captures the connection capability", async () => {
  const { server, session } = await openSession();

  assert.equal(session.connectionToken, "conn-1");
  const hello = server.requests[0];
  assert.ok(hello);
  assert.equal(hello.body["type"], "hello");
  // The hello request precedes the capability, so it carries no connection header.
  assert.equal(hello.headers["liasse-connection"], undefined);
});

test("subscribe binds a view to the ephemeral stream-session over an authenticated POST", async () => {
  const { server, session } = await openSession();
  const sub = session.subscribe("public.tasks");
  await sub.ready;

  const view = server.requests.at(-1);
  assert.ok(view);
  assert.equal(view.body["type"], "view");
  assert.equal(view.body["sub"], sub.sub);
  assert.equal(view.body["address"], "public.tasks");
  // The view POST is authenticated (connection capability) AND carries the ephemeral
  // stream-session the server announced on the stream's first event — binding this
  // subscription's frames to that exact socket.
  assert.equal(view.headers["liasse-connection"], "conn-1");
  assert.equal(view.headers["liasse-stream"], server.lastStream);

  // The stream itself was opened ANONYMOUSLY: its request carries only the URL — no
  // token, no capability that anyone could present to steal it.
  assert.equal(server.streams.length, 1);
  assert.deepEqual(Object.keys(server.stream.request), ["url"]);
});

test("an init then a patch fold through the wasm replica into the store", async () => {
  const { server, session } = await openSession();
  const sub = session.subscribe("public.tasks");
  await sub.ready;

  const states: ViewState[] = [];
  sub.subscribe((state) => states.push(state));
  // The store contract delivers the current (empty) snapshot immediately.
  assert.equal(states.at(-1)?.rows.length, 0);

  // init at f0: two rows in view order.
  server.stream.push(
    initFrame(sub.sub, [
      { id: "a", value: { title: "buy milk" } },
      { id: "b", value: { title: "eggs" } },
    ]),
    "f0",
  );
  assert.deepEqual(sub.rows.map((row) => row.id), ["a", "b"]);
  assert.equal(states.at(-1)?.frontier, "f0");

  // patch at f1: insert c at the end (current-result index 2). §12.2 → [a, b, c].
  server.stream.push(patchFrame(sub.sub, [{ op: "insert", at: 2, id: "c", value: { title: "bread" } }]), "f1");
  assert.deepEqual(sub.rows.map((row) => row.id), ["a", "b", "c"]);
  assert.deepEqual(
    sub.rows.map((row) => (row.value as { title: string }).title),
    ["buy milk", "eggs", "bread"],
  );
  assert.equal(states.at(-1)?.frontier, "f1");
  assert.equal(sub.closed, false);
});

test("a scalar frame is readable as the store's scalar value", async () => {
  const { server, session } = await openSession();
  const sub = session.subscribe("public.count");
  await sub.ready;

  server.stream.push(scalarFrame(sub.sub, 41), "f0");
  assert.equal(sub.scalar, 41);
  assert.equal(sub.rows.length, 0);
});

test("a reset reopens every live subscription from scratch", async () => {
  const { server, session } = await openSession();
  const sub = session.subscribe("public.tasks");
  await sub.ready;
  server.stream.push(initFrame(sub.sub, [{ id: "a", value: { title: "one" } }]), "f0");
  assert.equal(server.viewCount(sub.sub), 1);

  // reset: the replica is dropped; the shell must re-POST the view.
  server.stream.push(resetFrame("unknown-connection"), "");
  await tick();
  assert.equal(server.viewCount(sub.sub), 2, "the subscription was reopened after reset");
  assert.equal(sub.rows.length, 0, "the replica is empty until the fresh init arrives");

  // fresh init re-establishes the subscription.
  server.stream.push(initFrame(sub.sub, [{ id: "z", value: { title: "again" } }]), "f2");
  assert.deepEqual(sub.rows.map((row) => row.id), ["z"]);
});

test("a close surfaces the reason and terminates the subscription", async () => {
  const { server, session } = await openSession();
  const sub = session.subscribe("public.tasks");
  await sub.ready;
  server.stream.push(initFrame(sub.sub, [{ id: "a", value: { title: "one" } }]), "f0");

  const states: ViewState[] = [];
  sub.subscribe((state) => states.push(state));
  server.stream.push(closeFrame(sub.sub, "unsubscribed"), "f1");

  assert.equal(sub.closed, true);
  assert.equal(states.at(-1)?.closed, true);
  assert.equal(states.at(-1)?.closeReason, "unsubscribed");
  assert.equal(sub.rows.length, 0, "a closed subscription exposes no rows");
});

test("a malformed frame becomes a handled error, never an uncaught throw", async () => {
  const { server, session } = await openSession();
  const sub = session.subscribe("public.tasks");
  await sub.ready;
  server.stream.push(initFrame(sub.sub, [{ id: "a", value: { title: "one" } }]), "f0");

  const errors: ConnectError[] = [];
  sub.subscribe(
    () => {},
    (error) => errors.push(error),
  );

  // Not JSON at all — the wasm core refuses it; the shell catches and delivers it.
  server.stream.push("not a frame", "f1");
  // A patch for a subscription the client never opened is refused, not invented.
  server.stream.push(patchFrame("ghost-sub", []), "f2");

  assert.equal(errors.length, 2, "both hostile frames surfaced as handled errors");
  assert.ok(errors.every((error) => error.kind === "protocol"));
  // The live subscription is untouched by the rejected frames.
  assert.deepEqual(sub.rows.map((row) => row.id), ["a"]);
});

test("a fault frame is delivered as a handled FaultError", async () => {
  const { server, session } = await openSession();
  const sub = session.subscribe("public.tasks");
  await sub.ready;

  const errors: ConnectError[] = [];
  session.onError((error) => errors.push(error));
  server.stream.push(JSON.stringify({ type: "fault", code: "bad-token", message: "forged" }), "");

  assert.equal(errors.length, 1);
  const fault = errors[0];
  assert.ok(fault instanceof FaultError);
  assert.equal(fault.code, "bad-token");
});

test("call attaches the client-seeded operation-id header and parses the outcome", async () => {
  const { server, session } = await openSession();

  const outcome = await session.call("public.tasks.add", { title: "x" }, { operationId: "op-7" });
  const request = server.requests.at(-1);
  assert.ok(request);
  assert.equal(request.body["type"], "call");
  assert.equal(request.headers["liasse-operation-id"], "op-7");
  assert.equal(outcome.status, "committed");

  // Without an explicit id the shell mints one (crypto.randomUUID()).
  await session.call("public.tasks.add", { title: "y" });
  const minted = server.requests.at(-1)?.headers["liasse-operation-id"];
  assert.ok(minted && minted.length > 0 && minted !== "op-7");
});

test("fetch, operation, and manifest parse their replies", async () => {
  const { server, session } = await openSession();

  const value = await session.fetch("public.tasks");
  assert.deepEqual(value, { title: "snapshot" });

  const status = await session.operation("op-7");
  assert.equal(status.status, "unknown");

  const manifest = await session.manifest();
  assert.deepEqual(manifest, ["public.tasks"]);
});

test("unsubscribe posts the unsubscribe frame with the connection header", async () => {
  const { server, session } = await openSession();
  const sub = session.subscribe("public.tasks");
  await sub.ready;

  await sub.unsubscribe();
  const request = server.requests.at(-1);
  assert.ok(request);
  assert.equal(request.body["type"], "unsubscribe");
  assert.equal(request.body["sub"], sub.sub);
  assert.equal(request.headers["liasse-connection"], "conn-1");
});

test("a dropped stream reconnects, gets a fresh session, and re-binds subscriptions", async () => {
  const server = new MockServer();
  const reconnects: (() => void)[] = [];
  const session = await connect("http://liasse.test", {}, {
    core,
    fetch: server.fetch,
    eventSource: server.eventSource,
    // Capture the reconnect job so the test fires it without a real timer.
    schedule: (run) => {
      reconnects.push(run);
      return () => {};
    },
  });

  const sub = session.subscribe("public.tasks");
  await sub.ready;
  server.stream.push(initFrame(sub.sub, [{ id: "a", value: { title: "one" } }]), "f0");
  assert.deepEqual(sub.rows.map((row) => row.id), ["a"]);

  const firstStream = server.lastStream;
  assert.equal(server.requests.at(-1)?.headers["liasse-stream"], firstStream, "bound to the first socket");
  const viewsBefore = server.viewCount(sub.sub);

  // The socket gives up. The shell schedules a reconnect (captured, not timed).
  server.stream.emitError(READY_CLOSED);
  assert.equal(session.connectionState, "reconnecting");
  assert.equal(reconnects.length, 1);

  // Fire it → a NEW socket, a NEW ephemeral session announced on it, and the shell
  // re-binds the live subscription to that session over a fresh authenticated POST.
  reconnects[0]?.();
  await tick();
  await sub.ready;

  const secondStream = server.lastStream;
  assert.notEqual(secondStream, firstStream, "each (re)connect is a distinct ephemeral session");
  assert.equal(server.viewCount(sub.sub), viewsBefore + 1, "the subscription was re-bound");
  assert.equal(server.requests.at(-1)?.headers["liasse-stream"], secondStream, "bound to the new socket");

  // A fresh init on the new socket re-establishes (replaces) the rows.
  server.stream.push(initFrame(sub.sub, [{ id: "z", value: { title: "again" } }]), "g0");
  assert.deepEqual(sub.rows.map((row) => row.id), ["z"]);
});
