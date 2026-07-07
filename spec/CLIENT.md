# The client protocol

Status: standard. How frontends talk to a Liasse engine: engine-served
client, names-only requests, the untrusted pipeline, and the transactional
live API.

## 1. The client is served by the engine

```html
<script type="module" src="/.liasse/client.js"></script>
```

The client library is an artifact of the engine — versioned with it,
served by it. Protocol, serialization, and hash routines ship together;
there is no independently-installed client to drift. (The JS library is
the reference embodiment; any client implementing this protocol behaves
identically.)

## 2. The surface manifest: the API is the grants

On connect, a session receives its **surface manifest**: the named views
and mutations the actor's grants expose, each with its declared parameter
shape. That manifest *is* the entire client API — the permission system
does not filter the client's power, it constitutes it. Typed bindings can
be generated from the manifest; even untyped, nothing outside it is
speakable.

## 3. Names only: declared entries, untrusted values

Expressions live in the model and execute with definer authority. The
wire carries names and values, never expressions:

```hjson
"my_shifts": {
  "$params": { "month": "month_t" },
  "$view": ".shifts[:s | s.member == actor && s.month == @month] { date, minutes, note }"
},
"add_justificatif({ label: label_t, file: pdf_t })": ".uploads + { label: @label, file: @file }"
```

Surface views take `$params` like mutations do, so filtering runs
server-side under the definer's expression.

The untrusted pipeline, normative, in order:

```text
1  name        must exist in the session's manifest — unknown: reject
2  shape       params parsed against declared types under wire rules;
               unknown fields reject; missing non-optional reject;
               numbers arrive as strings or reject
3  normalize   $normalize per param, then $check per param — reject
               with the declared message
4  blobs       streamed; size-capped at $max_bytes as bytes arrive;
               media validated; sha512 computed; on success the param
               binds to the descriptor — the body never sees raw bytes
5  bind        params bound as typed values into definer expressions;
               client input is never evaluated or spliced
6  authorize   actor membership re-verified before admission
7  invariants  meters, checks, guards — reject before admission on failure
```

## 4. Four wire verbs

```text
connect      token -> session -> surface manifest
view         name + params -> init(frontier) + patches + call reports
call         name + params (+ blob streams) -> committed | unchanged | rejected
fetch plan   descriptor -> permitted holders + access, in $serve order
```

Nothing else is speakable from the outside.

## 5. Views are live by default

```text
view "my_shifts" { month: "2026-06" }
  <- init   full result at a frontier
  <- patch  ops keyed by row identity, each tagged with its commit id
  <- report committed/unchanged/rejected notices for this session's calls
```

The client maintains local state defined as the fold at the tracked
frontier and resumes from it on reconnect — client and server cannot
structurally disagree about which version is displayed. Coherence is not
a best effort; it is the definition of the local copy.

## 5b. Windows: client policy, engine delivery

The split is strict. **Window policy is a frontend concern**: what size,
where the anchor sits, how scroll and resize behave. **Window delivery is
the engine's**: given the declared window, the back→front stream carries
only the necessary rows — never the whole view.

A window is a parameter set on a view subscription:

```text
{ $size: n }                                    fixed slice by position
{ $size: n, $anchor: <row-key> }                slice containing the anchor
{ $size: n, $anchor: <row-key>, $slide: true }  anchor kept centered
{ $size: n, $anchor: $first | $last }           edge-pinned (live tail)
```

The engine evaluates the slice at each frontier and emits diffs for the
slice only — `enter(row, pos)`, `exit(key)`, `shift`, `re-anchor(key)`,
`range-update { page? | center, of_total }` — each tagged with its commit
id, resumable by frontier. Positional windows are `$skip`/`$limit` under
the hood; anchored windows the engine keeps glued to the row, not the
position: inserts above a centered anchor push the top edge out, symmetric
below; a deleted or filtered-out anchor re-anchors to the nearest survivor
with a notice.

The client library translates UX into re-parameterization, incrementally:

```text
scroll            window.anchor(key)  or adjust $skip
resize            window.resize(n') — the focal row (anchor, else first
                  visible) stays in the window: sliding grows/shrinks
                  symmetrically (spilling at view ends), edge-pinned
                  moves only the far edge, fixed keeps the page
                  containing the focal row
```

Both are frontier-atomic with that frontier's data diff, and emit only
edge enters/exits plus one range update — existing rows never re-render,
nothing refetches, no mechanism disagrees with another about positions.
Permissions compose (a window scopes an already surface-gated view);
counts and ranges come from the same fold as the rows, so they are never
mutually stale. Surface-declared `$limit` caps `$size`; an invalid anchor
at subscribe degrades to edge-pinned with a notice.

## 6. Every call is a transaction, live-first

One operation per call: it resolves when the whole effect is committed or
known unchanged, rejects with the reason before admission, and cancellation —
explicit or by disconnect — means nothing happened.

```text
op = call "add_justificatif" { label, file: <stream> }
op.progress -> { phase: hashing | uploading | replicating | committing,
                 sent, total }
op.cancel()
await op    -> committed, unchanged, or rejected — never a half-state
```

For calls with blob parameters: the client hashes while streaming to the
placement plan's first store (presigned where the connector allows); the
engine verifies sha512 + size against the landed object; only then does
the mutation apply. Bytes and rows are one atomic outcome; disconnect
mid-stream leaves no descriptor and no commit.

## 7. Downloading: plan, race, verify

```text
op = fetch(descriptor)
```

The engine returns the fetch plan — verified holders the session may
access, in `$serve` order, presigned or proxied per connector. The client
then does the transport work:

```text
probe        ranged reads against candidates -> latency/throughput
choose       best holder wins; slower ones remain as spares
stream       ranged, resumable, progress events
dual-wield   when ranges are supported and one pipe underdelivers,
             split the byte range across holders and interleave
             (default on; client-config kill switch)
verify       sha512 computed incrementally; a mismatching source is
             dropped mid-stream, its ranges refetched from a spare —
             the result is certified to hash to $sha512 or the op fails
```

Application servers proxy bytes only when a connector cannot presign;
the API shape is identical either way.

## 8. Trusted and untrusted clients

There are exactly two client classes:

```text
trusted     the embedder and operator tooling: full expression access —
            its own views, its own mutations, the system session or
            expression-capable actor sessions it opens
untrusted   frontends: constituted entirely by the surface manifest —
            names and typed values only, no custom requests, ever
```

There is no third tier. Admin consoles and BI are trusted clients;
anything reachable by end users is untrusted and speaks the four verbs
of §4.

## 9. Boundaries

```text
uploads/downloads outside a call/fetch op do not exist — there is no
  raw byte endpoint
the manifest is per session: role changes take effect on the next
  manifest refresh; authorization is re-verified before admission
  regardless
offline mutation queues are an application choice built on the same
  ops: queued calls settle as committed, unchanged, or rejected when
  connectivity returns, with reports on the view stream
```
