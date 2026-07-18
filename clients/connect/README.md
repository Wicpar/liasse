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

## Transport — ephemeral, unstealable SSE sessions

There is **no presentable credential that grants access to a stream**. The stream is
opened **anonymously**; the server mints a fresh, single-socket **stream-session** and
announces its id on the stream's first event; the client then attaches its subscriptions
to that session with **authenticated POSTs**. Opening the URL only yields a new, empty
session — you cannot attach to someone else's — and a URL token, a cookie, or a log
line reveals nothing that lets a stream be stolen.

- **Downstream** is one Server-Sent-Events stream per connection (`EventSource`), opened
  `new EventSource(url)` — no token in the URL, no cookie relied on. Its first event is a
  `liasse-session` event carrying `{ "stream": "<id>" }` (kept distinct from the
  `message` events that carry §12.2 wire frames, so the wasm core only ever sees frames).
  The SSE `id:` is the §12.2 frontier token. Never WebSockets.
- **Upstream** is `fetch` POSTs carrying the tagged request body the wasm core produced,
  with the `Liasse-Connection` capability header (auth), the `Liasse-Stream` header (the
  ephemeral stream-session, binding this request's downstream frames to the socket), and,
  on a `call`, the `Liasse-Operation-Id` header (§12.3). Auth and binding both ride
  request headers — never a URL, so nothing leaks to history, logs, or `Referer`.

Why this resists theft: the stream-session id lives only in the first event of a live
socket (ephemeral, never re-transmitted). Stealing it is useless — attaching a
subscription requires an authenticated POST, and the server binds each stream-session to
the connection that first used it, so you can neither attach your subscriptions to a
victim's socket nor read a victim's socket with your connection. Covered by the tests
`the default transport opens the stream anonymously — no token or credential` and
`subscribe binds a view to the ephemeral stream-session over an authenticated POST`.

### Auto-reconnect & bad networks

- A native `EventSource` reconnects on a transient drop (readyState stays `CONNECTING`);
  the shell waits for it rather than open a second stream.
- If the source gives up (readyState `CLOSED`), or an injected transport does not
  self-reconnect, the shell **rebuilds it with exponential backoff** (base 1s, cap 30s,
  jittered; customizable via `connect(..., { schedule })`). A frame, session, or `open`
  resets the backoff.
- Every (re)connect is a **new ephemeral stream-session** announced on the new socket;
  the shell **re-binds every live subscription** to it with fresh authenticated POSTs,
  and the §12.2 `init` frames that follow re-establish the rows. A server `reset` frame
  likewise re-establishes all subscriptions. The stream self-heals; nothing throws on a
  bad network.
- Observe it: `session.connectionState` and `session.onConnectionState(cb)` report
  `connecting | open | reconnecting | closed`, so an app can show a "reconnecting…"
  indicator.

> **Reference-server note.** This model needs the server to (1) announce a fresh
> stream-session on the anonymous SSE stream's first event and (2) accept the
> `Liasse-Stream` header on `view` (and coherence-bearing) POSTs, binding each
> stream-session to the connection that presents it. The current S2 reference binding
> (`crates/liasse-connect/src/bind/std_http.rs`) instead identifies the SSE stream by a
> `liasse-connection` request header on the GET — which a native browser `EventSource`
> cannot send, and which this model deliberately removes. So a browser deployment needs
> that server binding updated to the announce-and-bind flow. The shell keeps the
> transport injectable; the node integration test models the intended server.

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
  (state) => render(state),             // fires with the current snapshot, then on each change
  (error) => report(error),            // faults and (re)subscribe failures, handled — never a throw
);
await tasks.ready;                       // optional: resolves when the server opened the view
tasks.snapshot().status;                 // "pending" | "open" | "closed" | "failed" (see below)
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

### Store contract: `ViewState.status`

`subscribe()` returns synchronously, but the view opens asynchronously — so the store
snapshot carries an always-correct `status` a non-awaiting reactive consumer (Vue, React,
Svelte) can branch on without racing `ready`:

- `pending` — subscribed, but no `init`/`scalar` has arrived yet. `rows` is `[]` because
  the view is **still loading**, which is distinct from an empty view.
- `open` — the first `init`/`scalar` frame arrived; `rows`/`scalar` are the authoritative
  view and **may legitimately be empty** (`{ status: "open", rows: [] }`).
- `closed` — the subscription terminated (`close`/`unsubscribe`); `closeReason` says why.
- `failed` — the `view` request was refused; `error` carries the reason. The refusal is
  put IN the store state (and broadcast to state listeners), not only to the error
  listener, so a consumer that only renders state still sees it.

`open`/`closed` are derived from the authoritative wasm replica, so they never disagree
with the rows. A `reset` clears the replica and the shell re-opens the view, so the status
drops back to `pending` until the fresh `init` — never a stale `open`.

The client is deliberately dumb: on a §12.2 `reset` it reopens its subscriptions from
scratch; on a `close` it surfaces the reason through the store state; on a `fault` it
delivers a handled `FaultError`. A malformed or forged downstream frame the wasm core
refuses becomes a handled `ProtocolError` on the error path — it never throws out of the
stream callback.
