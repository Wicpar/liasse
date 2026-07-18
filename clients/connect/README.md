# @liasse/connect

The untrusted browser client for the Liasse client-sync connector (SPEC §12). A thin
transport + ergonomics layer over the `liasse-connect-wasm` core:

- the **wasm core** owns every wire semantic — §12.2 patch-apply, frame decode, and
  request-body encode all live in Rust (`crates/liasse-connect-wasm` → `liasse-wire`);
- this **TypeScript shell** adds only the EventSource lifecycle, `fetch` POSTs with
  capability-header attachment, a store-like subscription surface, and reset →
  re-subscribe.

It holds **no authority**. Every capability (connection token, frontier token,
operation id, occurrence id) is opaque data the client carries and echoes back; all
authorization, projection, and token minting stay server-side.

## Transport

The **SSE channel is anonymous**. It carries no authorization of its own: authority is
established and re-checked on the POSTs (`hello` authenticates the connection; `view`
opens/re-verifies a subscription; `unsubscribe` closes it). The connection token is
just an opaque **stream handle** that links the SSE stream to its connection.

- **Downstream** is one Server-Sent-Events stream per connection (`EventSource`). The
  connection handle rides the **URL** (a native `EventSource` has no other channel and
  needs none — the stream carries no secret), so a plain browser `EventSource` works
  with no headers. The SSE `id:` is the §12.2 frontier token. Never WebSockets.
- **Upstream** is `fetch` POSTs carrying the tagged request body the wasm core produced,
  with the `Liasse-Connection` handle header and, on a `call`, the `Liasse-Operation-Id`
  header (§12.3). POSTs can set headers, so the capability travels there.

### Auto-reconnect & bad networks

- A native `EventSource` reconnects on a transient drop and replays `Last-Event-ID`
  automatically (readyState stays `CONNECTING`); the shell waits for it rather than
  open a second stream — resume is **free**.
- If the source gives up (readyState `CLOSED`), or an injected transport does not
  self-reconnect, the shell **rebuilds it with exponential backoff** (base 1s, cap 30s,
  jittered), carrying the last observed frontier so a fresh source still resumes (§12.2).
  A frame or `open` resets the backoff.
- If the server cannot replay the retained range it sends `reset`; the shell then
  **re-subscribes every live view from scratch** (§12.2). The stream self-heals and
  nothing throws on a bad network.
- Observe it: `session.connectionState` and `session.onConnectionState(cb)` report
  `connecting | open | reconnecting | closed`, so an app can show a "reconnecting…"
  indicator.

> **Reference-server note.** The intended browser model above reads the connection
> handle (and `last-event-id` on a manual rebuild) from the **SSE URL**. The current S2
> reference binding (`crates/liasse-connect/src/bind/std_http.rs`) instead reads
> `liasse-connection` / `last-event-id` from request **headers** on the GET and ignores
> the URL. So against that specific server today, either update it to also read the URL
> handle, or inject a header-capable `EventSourceFactory` via
> `connect(..., { eventSource })`. This shell keeps the transport injectable for exactly
> that reason; the node integration test models the intended URL-handle server.

## Build

The wasm core is a generated artifact, never committed (`.gitignore`). Build it first:

```sh
npm install
npm run build:wasm     # wasm-pack build → wasm/web (browser) and wasm/node (tests)
npm run build          # tsc → dist/
```

`build:wasm` requires the Rust toolchain, `wasm-pack`, and the `wasm32-unknown-unknown`
target.

## Scripts

| script            | what it does                                             |
| ----------------- | ------------------------------------------------------- |
| `build:wasm`      | `wasm-pack build` the core into `wasm/web` + `wasm/node` |
| `typecheck`       | `tsc --noEmit` (strict)                                  |
| `build`           | `tsc` → `dist/`                                          |
| `test`            | build, then the node integration test over the real core |

## API

```ts
import { connect } from "@liasse/connect";

// Open the connection (§11). In a browser the wasm core, fetch, and EventSource are
// the defaults; pass overrides via the third argument to inject them.
const session = await connect("https://app.example/liasse", { auth });

// Subscribe to a live view (§12.2). Returns a store-like handle immediately; it fills
// as init/patch frames arrive over the SSE stream.
const tasks = session.subscribe("public.tasks", { params: { open: true } });
const stop = tasks.subscribe(
  (state) => render(state.rows),        // fires with the current snapshot, then on each change
  (error) => report(error),            // faults and (re)subscribe failures, handled — never a throw
);
await tasks.ready;                       // optional: resolves when the server opened the view
tasks.rows;                              // current rows: { id, value }[]
tasks.scalar;                            // scalar/aggregate value, or null
tasks.closed;                            // terminal state; state.closeReason says why

// Invoke a call (§10, §12.3). The operation id is the client-seeded idempotency
// capability — pass one to make a retry safe, or let the shell mint crypto.randomUUID().
const outcome = await session.call("public.tasks.add", { title: "buy milk" }, { operationId });

// One-shot read at the current frontier (§12.1).
const snapshot = await session.fetch("public.tasks");

// Retained operation status (§12.3), the manifest (§12.1), and teardown.
const status = await session.operation(operationId);
const manifest = await session.manifest();
await tasks.unsubscribe();
session.close();
```

The client is deliberately dumb: on a §12.2 `reset` it reopens its subscriptions from
scratch; on a `close` it surfaces the reason through the store state; on a `fault` it
delivers a handled `FaultError`. A malformed or forged downstream frame the wasm core
refuses becomes a handled `ProtocolError` on the error path — it never throws out of the
stream callback.
