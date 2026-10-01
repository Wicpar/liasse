# Connect

The alpha's client binding, `liasse-connect`, and its web client,
`liasse-connect-wasm` with `clients/connect` ([ALPHA.md](ALPHA.md)).
SPEC chapter 10 states what a client reads and calls, and what it can rely on;
this document states how the alpha's client does it: how the binding carries
connections over HTTPS, its operations and their messages, and how the web
client behaves, its key functions included.

---

## 1. Transport

The host serves the binding under a base URL over HTTPS.

| Request | Does |
| --- | --- |
| `POST {base}/connections` | opens a connection, and returns its connection token and its entry points: the views that the host's `authorize` gives it (ALPHA.md), each as `{ view: <handle>, shape }` |
| `GET {base}/stream` | the connection's stream of frames, as Server-Sent Events |
| `POST {base}/request` | one operation in, one reply out |
| `DELETE {base}/connections` | closes the connection |

- **The connection token** is 32 random bytes, in unpadded base64url, and
  every request after the first carries it in the header `Liasse-Connection`.
  It never appears in a URL, a log, or history.
- **Bodies** are one CBOR item each (chapter 2), with the media type
  `application/cbor`.
- **The stream** is `text/event-stream`. Each event carries one frame: its
  `id` is the frame's stream position, and its `data` is the frame's CBOR item
  in unpadded base64url. After a drop, the client reattaches with
  `Last-Event-ID`, and the server sends every frame after that position while
  the connection lasts. A connection lasts through a reattachment within the
  grace period that the host sets; after it, the server closes the connection.
- **Replies** are HTTP 200 for every outcome, a denial included. A **transport
  fault**, a message that the server fails to process, is an HTTP error with an
  empty body: 400 for a message that fails to parse, 413 for one past the host's
  size limit, and 410 for a closed connection.
- **Calls.** A call is open while its request is (SPEC §10.3): the server
  cancels a call whose request or connection closes before its commit, and sends
  the reply of a call on its request alone.
- **Blob content** travels between the client and the storage locations through
  presigned requests. An `upload` plan gives a presigned `PUT` for each
  descriptor, and a fetch plan gives presigned `GET`s (chapter 2). A presigned
  request is a capability until its expiry: one write of one size at one place,
  or reads of one content; the binding logs none.
- **Order.** The server writes every frame through a commit to the stream before
  it sends the reply of the call that committed it (2.4).

---

## 2. Messages

Every message is one CBOR item in the alpha's encoding (ALPHA.md): a map with
text keys. A message that fails to parse as one operation is a transport fault.

The server mints the **opaque tokens**: frontiers, stream positions, commits,
the handles of entry points, and occurrence tokens, each a byte string. The
client stores them, compares them for equality, orders occurrence tokens (2.4),
and sends them back unchanged. The server refuses, before anything runs, a token
that it did not mint for that use on that connection: a frontier as a released
range (2.4), and every other token with the denial (SPEC §10.2). The client
chooses subscription names.

### 2.1 Targets

A **target** names what a read or a call names, in the steps of SPEC §10.2:

```text
{ entry: <handle>, steps: [ step, … ], stages? }
```

`entry` is the handle of an entry point. A step is a member's name as a text, a
view with its arguments as `[name, { args }]`, or an element as its occurrence
token (2.4), which names its occurrence on the connection that received it
alone. `challenge` is `{ entry: h'…', steps: ["challenge"] }`, and
`me / tasks / t1 / complete` is
`{ entry: h'…', steps: ["tasks", h'…', "complete"] }`. `stages`, source text
such as `filter(!.done) |> take(20)`, makes the target a view that the client
writes over the view that the steps reach (SPEC §10.2): the server parses it as
hostile input, and checks that it reads what the client reaches and calls the
functions of the host libraries that serve clients, before anything runs. A
`stages` text that fails to parse rejects the read with the code `malformed`.

### 2.2 Operations

| Operation | Message | Does |
| --- | --- | --- |
| `describe` | `{ op: "describe" }` | describes the members of the connection's entry points |
| `watch` | `{ op: "watch", name, target, frontier? }` | reads a view live, as the subscription `name` (2.4); `frontier` resumes it |
| `read` | `{ op: "read", target }` | reads a view once |
| `unwatch` | `{ op: "unwatch", name }` | ends the subscription `name` |
| `call` | `{ op: "call", target, args }`, or `{ op: "call", list: [ { target, args }, … ] }` | calls a mutation, or a list of them (SPEC §10.3) |
| `upload` | `{ op: "upload", target, args }`, or `{ op: "upload", list: [ { target, args }, … ] }` | plans the blob content of the call that the client makes next (2.5) |
| `fetch` | `{ op: "fetch", target, members: [ … ] }`: `target` reaches the row, and `members` is the path down to the blob | plans the read of a blob (2.5) |
| `release` | `{ op: "release", view: <handle> }` | releases an entry point, and ends its subscriptions |

- `args` is a map from input names to values.
- `call` alone changes state, one transaction for each message.
- `describe` gives, for each member of each entry point: its kind and name; each
  input or parameter, with its name, its type text, and whether it is required,
  optional, or defaulted; and its result, a snapshot or a view, with its type
  text or its shape. It describes the instance's active version.
- A **type text** is a type's definition after elaboration, with every argument
  written out, such as `Text?`, `Text*`, or `{ title: Text; done: Bool }*`. A
  storage name belongs to its member (SPEC §12.2), and the type text of the
  member's type leaves it out.

### 2.3 Replies

| Reply | Message |
| --- | --- |
| committed | `{ outcome: "committed", frontier, commit, result }`, or `results` for a list |
| unchanged | `{ outcome: "unchanged", frontier, result }`, or `results` |
| rejected | `{ outcome: "rejected", code, message?, index?, path?, trace? }` |
| denied | `{ outcome: "denied", code: "denied", message: "Request denied" }` |
| failed | `{ outcome: "failed", code: "unavailable" }` |
| watch | `{ outcome: "watching", name }`; the frames follow on the stream |
| read | `{ outcome: "read", frontier, occurrences }` or `{ outcome: "read", frontier, value }` |
| describe | `{ members: [ … ] }`, each with the information of 2.2 |
| upload | `{ outcome: "plan", uploads: [ { blob, places: [ { url, headers, expires }, … ] }, … ] }` |
| fetch | `{ outcome: "plan", blob, sources: [ { url, expires }, … ] }` |

- A call's `result` is its snapshot as a value, or `{ view: <handle>, shape }`
  for a returned entry point.
- An upload plan has one entry for each distinct descriptor among the `Blob`
  arguments. Each place is a presigned `PUT` to `url` that carries each header
  of `headers` exactly and a body of the descriptor's `size`, before `expires`.
- A fetch plan lists at least one source, each a presigned `GET` to `url` before
  `expires`; with no source, the reply is `failed` with the code `unavailable`.
- `expires` is a `Timestamp`.
- `frontier` in an outcome is a stream position: the connection's live views
  show the commit once the client has applied the frames through it.
- `index` names the failing mutation of a list. `message` comes when a module
  wrote it, and a generated `message`, `path`, and `trace` in development mode
  (SPEC chapter 7, ALPHA.md).

### 2.4 Subscriptions

A `watch` opens a **subscription** to a view, under the name that the client
gives: the view's result at a frontier, then its advances. A **frontier** is an
opaque position in one subscription's stream, which covers commits and instants
(SPEC §5.2). The first frontier of a subscription includes every commit that the
connection received as `committed` before the `watch`.

Each element of a result of cardinality `*` or `+` is an **occurrence**,
identified by the view declaration that yields the result and by its **path**:
the rows that it was reached through, from the outermost, then the row that it
comes from, the key of its `group` (ALPHA.md), or else its value and its rank
among the equal elements of the result, as a task of `.accounts.tasks` is
reached through its account. Two occurrences of one result have different paths,
and an occurrence keeps its path at every read. Each occurrence is named in the
stream by an **occurrence token**. Within one stream, from an `init` to the next
`init` or the `close`, an occurrence keeps one token, and a token names one
occurrence, also after the occurrence leaves the result and comes back. Across
the streams of one connection, a token names one occurrence.

The occurrence token of an occurrence that comes from a row is ordered. For each
row of its path, outermost first, it holds the instant of the commit that
inserted the row, in microseconds since the Unix epoch, as 8 bytes; the first 8
bytes of HMAC-SHA-256, under a 32-byte key that the store draws when it creates
the application, of the store's identifier of that commit; and the row's index
among the insertions of that commit, as 4 bytes; each big-endian. These tokens
sort, byte by byte, in the order of the instants at which their rows were
inserted: insertion order (SPEC §4.3), save between rows that concurrent commits
insert, or that commits insert as the clock moves back (SPEC §6.6). A token so
shows the instants at which the rows of its path were inserted. The server mints
the token of every other occurrence, such as an element of the set that `group`
creates, when it first gives it, and these tokens sort in the order in which it
first gave them.

```text
{ frame: "init",  name, frontier, occurrences: [ [token, value], … ] }
{ frame: "init",  name, frontier, value }
{ frame: "patch", name, frontier, ops: [ op, … ] }
{ frame: "value", name, frontier, value }
{ frame: "close", name, frontier, reason }
```

- `init` gives the complete result: each occurrence with its token and its
  value, for a view of cardinality `*` or `+`, and the value of a view of
  cardinality one or `?`.
- `patch` gives the operations that turn the previous result into the result at
  its frontier: `["insert", token, value]` inserts the occurrence,
  `["remove", token]` removes it, and `["update", token, value]` replaces its
  value, keeping its token.
- `value` gives the new value of a view of cardinality one or `?`, at a frontier
  where it changed.
- `close` ends the subscription, with its reason.

A value holds what the view's shape presents, and a set of cardinality `*` or
`+` inside it is an array of `[token, value]` occurrences.

The operations of a patch apply in order, each to the result that the ones
before it leave. After the last one, the client's result equals the view at the
new frontier: the same occurrences, with the same values.

- An occurrence in the result at both frontiers keeps its token and changes only
  through `update`.
- A patch covers one or more commits and instants, and a patch of zero
  operations advances the frontier alone.
- A patch that names an occurrence outside the result, or inserts one already in
  it, is inconsistent: the client drops the subscription's result and watches
  again.

Every frame of one subscription is delivered, in order, and frames of different
subscriptions can interleave. A server that fails to deliver a frame, as to a
client that reads too slowly, closes the subscription with `server-closed`. The
client then watches again under the same name, presenting the last frontier that
it applied, and the server answers with the later frames of the stream, or with
a fresh `init` when it has released them. After a fresh `init`, the client
replaces the result that it held, and its tokens can differ.

At each frontier, the server resolves each subscription's target again
(SPEC §10.2). When the view is out of reach, the frame at that frontier is a
`close` for `unreachable`, and the client drops the result that it holds.

| Reason | When |
| --- | --- |
| `unwatched` | the client unwatched the subscription |
| `unreachable` | its target left the client's reach: the entry point was released, the row that it was reached through left its result, or an update removed the view |
| `replaced` | the client watched another target under the same subscription name |
| `server-closed` | the server ended it: its evaluation failed at a later frontier, the server failed to deliver a frame, or the connection closed |

A `watch` under the name of an open subscription replaces it, or resumes it when
it names the same target with the last frontier that the client applied. The
server resolves a resumed subscription again before any frame flows. A frontier
that the server did not issue for that stream is answered with a fresh `init`.

An update keeps the subscriptions to the views that remain: at the update's
frontier, they are read under the new version and patched, and their occurrences
keep their tokens.

For a call whose outcome is `committed`, the server, in this order:

1. commits durably;
2. makes the commit visible to every read that starts after it;
3. resolves every subscription of the connection again, closing those out of
   reach;
4. advances the other subscriptions of the connection through the commit;
5. sends the reply.

Before it sends `unchanged`, the server advances the connection's subscriptions
to at least the returned frontier.

### 2.5 Blobs

`upload` resolves its calls, and reads and judges their arguments, as a call is
before its blob content arrives (SPEC §10.3), with the same denials and
rejections, and then gives the plan.

- The plan names, for each distinct descriptor, the locations whose connector
  gives write access for its size, in the order of the class that the input's
  storage name resolves to from the row or the root that holds the mutation, as
  a stored member's does (SPEC §12.2).
- The content that a connection's plans place serves the calls of that
  connection, and is kept while the connection lasts. The host limits the bytes
  that the plans of one connection place: an upload past the limit, and one with
  a descriptor that no location accepts, fail with the code `unavailable`.

`fetch` resolves its target as a read does (SPEC §10.2), checks that the view
shows the whole descriptor, and gives the plan.

---

## 3. The web client

- **The WASM core**, `liasse-connect-wasm`, decodes and encodes CBOR, keeps a
  replica of each subscription's result, in the order of its occurrence tokens,
  applies patches and checks their consistency (2.4), tracks frontiers and
  stream positions, computes the SHA-512 digests of blob content (ALPHA.md) in
  chunks, derives account keys from passwords and credential outputs and signs
  with them (chapter 4), and erases passwords, outputs, and seeds from its
  memory once used.
- **The TypeScript shell**, `clients/connect`, reads the stream with a
  streaming `fetch`, which carries the `Liasse-Connection` header, sends
  requests and the presigned requests of blob plans, calls WebAuthn (4.1, 4.3),
  stores the session key, and exposes subscriptions to the application's
  framework.
- **Sending a blob.** The client reads the file once to compute its digest and
  size, builds the descriptor, and sends `upload` with the calls that carry it.
  For each entry of the plan, it sends the content to the first place, and to
  the next place after a failure; a 412 means that the place already holds its
  write. Then it sends the `call` with the same arguments on the same
  connection. When the connection is gone before the call, it asks a new
  connection for a new plan and sends the content again.
- **Reading a blob.** The client sends `fetch`, reads the first source, and the
  next source after a failure, computes the digest as the content arrives, and
  hands the content to the application only once its digest and size match the
  descriptor.
- **Cross-origin access.** Every location of a class that a plan names answers
  CORS requests from the application's origin: `PUT` with the headers of its
  plans, and `GET`.
- **Recovery.** After a drop, the client reattaches to its stream. When the
  connection is gone, it opens a new one, the application makes again the calls
  that returned its entry points, as `resume` does in 4.4, and the client
  watches each subscription again with the last frontier that it applied. A
  patch that fails to apply drops that subscription's replica, and the client
  watches it again.
- **Outcomes.** The client presents `committed` once its replicas have applied
  the frames through the outcome's frontier. It reports a call whose request
  failed before its reply as lost: the call was cancelled, or it committed and
  its reply is lost (chapter 1).

---

## 4. Keys

A call's arguments reach the server, and what a module stores of them reaches
state, history, and exports (SPEC §14.1). The web client derives key pairs from
what a person knows or holds, and signs with them: the password, the
credential's output, the seed, and the private key stay in the web client, and a
call carries public keys, nonces, and signatures.

### 4.1 Account keys

The web client derives an Ed25519 key pair from a password, or from a WebAuthn
credential: a passkey or a hardware security key. One password, or one
credential, gives one key pair, on every device and at every derivation.

**From a password:**

- the password is prepared with the OpaqueString profile of RFC 8265 and
  encoded as UTF-8, so that one password typed on different devices gives one
  key;
- Argon2id (RFC 9106) with `m = 19456` KiB, `t = 2`, `p = 1`, and a 32-byte
  output, which is the Ed25519 seed;
- the salt is the UTF-8 application name, one zero byte, and the UTF-8 account
  name that the application gives.

A stored public key lets a reader of the store test guessed passwords, as a
stored password hash does, and Argon2id's cost slows each guess.

**From a credential**, through the `prf` extension of WebAuthn Level 3:

- the credential is discoverable (`residentKey: "required"`) and belongs to
  the application's relying party identifier; its `user.id` is 32 bytes that
  the client draws, and its `user.name` is the account name;
- every ceremony asks for user verification (`userVerification: "required"`),
  since a security key's output depends on user verification (CTAP 2.1
  `hmac-secret`);
- every ceremony evaluates the PRF with `first` set to the UTF-8 application
  name, and gives a 32-byte output, the same on every device that the
  credential syncs to;
- HKDF-SHA-256 (RFC 5869) of the output, with an empty salt, the info
  `liasse-connect account key` in UTF-8, and a 32-byte output, is the Ed25519
  seed;
- the ceremony's challenge is the one that the application gives (4.3), or 32
  bytes that the client draws.

At registration, the client evaluates the PRF in `navigator.credentials.create`,
and, when the reply holds no PRF result, runs `navigator.credentials.get` once
for the new credential. When a credential's reply has `prf.enabled` set to
`false`, the client takes a password or another authenticator. A key from a
credential derives from 32 bytes that the authenticator alone computes, out of
reach of a reader of the store.

### 4.2 Session keys

Each device holds a session key pair: a non-extractable WebCrypto Ed25519 key,
kept in IndexedDB.

### 4.3 Assertions

The client passes a challenge that the application gives to
`navigator.credentials.create` or `navigator.credentials.get` unchanged, and
gives the application the reply: `client_data` and `attestation_object` for a
registration, and the credential identifier, `client_data`,
`authenticator_data`, and `signature` for an assertion. One ceremony gives both
the assertion and the PRF output of 4.1. A module verifies them with the
`webauthn` library of [ALPHA.md](ALPHA.md).

### 4.4 Example: signing in

The module of SPEC §1.2 signs its clients in with these keys:

- **Sign-up.** The client derives the account key pair and calls
  `sign_up(email, name, public_key)`.
- **Sign-in.** The client calls `challenge()`, derives the account key pair,
  which asks for the password or runs the credential's ceremony, signs the
  nonce followed by the session's 32-byte public key, and calls
  `sign_in(email, nonce, session, signature)`. The returned entry point is the
  connection's access. An unknown account, a wrong password, and another
  account's credential fail alike.
- **Resuming.** A new connection, after a reload, a network change, or on
  another process, calls `challenge()`, signs the nonce with the session key,
  and calls `resume(session, nonce, signature)`, which returns the same view.
  The password or the credential is asked for again once the session ends.
- **Ending.** `sign_out`, a native removal of the session, and its expiry end
  the session: the entry point reads `none`, and every read and call through it
  is denied.

A module that approves single operations adds:

- **Approving one operation.** A mutation that needs the approval of one
  operation returns a challenge that it derives from the operation and a drawn
  nonce, and keeps it with the pending operation. The client derives the
  account key pair again, signs the challenge, and sends the signature to the
  mutation that confirms the operation, which verifies it over the kept
  challenge with the account's public key.
- **Authenticator signatures.** A module that needs the authenticator's own
  signature, with its user-verification flag and its signature counter, holds
  its relying party identifier and the application's origins, which the host
  writes natively. At registration, it takes the credential from
  `webauthn.credential` over a nonce of `challenge()`, and stores its
  identifier, public key, and counter. To approve an operation, it verifies the
  assertion with `webauthn.verify` over the kept challenge, requires
  `user_verified`, and, when either the stored or the new counter is nonzero,
  requires the new counter to exceed the stored one and stores it.
