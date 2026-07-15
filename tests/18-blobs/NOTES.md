# §18 Blobs — chapter notes

This file documents the step vocabulary and simulated host shape this chapter
adds beyond `tests/FORMAT.md`, plus the shape assumptions the cases make where
§18 leaves an internal representation to the implementation. Grounding: SPEC.md
§18 (blobs), §12.3 (operation status), §19.7 (export scope), Annex A.8 (key
eligibility), Annex B.4 (descriptor ordering/equality).

FORMAT.md lists `blob_put` / `blob_get` as §18 step keys but leaves their
payloads open; this file pins them, and adds the connector fault-injection and
reconciler steps the chapter needs.

## Simulated host components (`hosts`)

```hjson
hosts: {
  connectors: {
    "fs-a": {
      capabilities: ["stream_upload", "stream_download", "range_reads",
                     "server_side_copy", "checksum", "delete", "physical_usage"]
      available: true            // optional; default true
    }
  }
}
```

`hosts.connectors` maps a registered connector name (the value a `stores` row's
`connector` field carries, §18.3) to the capability set it advertises for the
load-time checks of §18.12 ("Loading validates connector capabilities required
by declared placement and client behavior"). A store row whose `connector`
names an unregistered connector, or a connector missing a capability the
declared placement/fetch behavior requires, fails validation before activation
(§18.12, §2.1).

## Staged content and declared descriptors

A blob upload carries a client-declared descriptor plus streamed bytes; the
runtime verifies the streamed bytes against the declared descriptor and against
the accepted blob type before admission (§18.2, §18.7). The corpus models the
streamed bytes as a UTF-8 string named by `content`. From that string the
harness computes the *true* `$sha512` (64 SHA-512 bytes, lowercase hex, §18.1)
and true `$bytes` (UTF-8 byte length). The client's declared descriptor is:

```
{ $sha512: <true hash>, $bytes: <true byte count>, $media: <media>, $name?: <name> }
```

unless a `claim` object overrides members to model a lying or malformed client
(a declared descriptor that disagrees with the streamed bytes, or that violates
the §18.1 descriptor value rules). `claim` members are inserted verbatim into
the declared descriptor and are the only way a case introduces a hash/byte/
media/format mismatch.

## Extension / pinned steps

### `blob_put`

```hjson
{ blob_put: {
    call: "public.docs.add"      // §18.7 mutation to admit
    param: "file"                // blob mutation parameter the descriptor binds to
    args: { id: "d1" }           // other (non-blob) mutation arguments
    content: "hello"             // staged bytes (UTF-8); harness hashes them
    media: "text/plain"          // declared $media
    name: "greeting.txt"         // optional declared $name
    claim: { $bytes: 999 }       // optional descriptor overrides (attacks)
    operation_id: "op-..."       // optional §12.3 operation identifier
    on: "c1"                     // connection; defaults per FORMAT.md
    bind: "d1"                   // optional: bind the committed descriptor value
  },
  expect: { outcome: ok, value: { ... } } }
```

`blob_put` and `blob_get` accept the same connection/authentication selectors
as `call` (`on`, and either a connection-level `authenticate` or a per-step
`auth: { auth, credential }`), since both are external requests over a surface.

Runs the complete §18.7 logical sequence: resolve target surface + auth, stage
and hash the bytes enforcing `$max_bytes`, bind the verified descriptor to
`param`, re-evaluate acceptance and placement **at admission**, create every
verified copy required by one complete policy branch, admit, and return the
mutation result. `bind` binds the committed descriptor for `$ref` reuse (its
`$sha512`/`$bytes`/`$media`/`$name` members). `expect` uses the standard
vocabulary: `rejected` for a failure that stops the call before its transition
is admitted (§18.2 final paragraph, §18.7).

### `blob_get`

```hjson
{ blob_get: {
    surface: "member.docs.file"  // a surface view resolving to a blob value
    args: { id: "d1" }           // view parameters
    at: ".file"                  // optional: descriptor occurrence within the row
    on: "c1"
  },
  expect: {
    outcome: ok
    holders: ["fs-a"]            // optional: verified holders in $serve order
    bytes: "the exact bytes"     // optional: the exact fetched result, given as
                                 //   the literal UTF-8 content that was staged
  } }
```

Requests a fetch plan for the blob value the surface exposes, re-evaluating
authentication, scoped role membership, surface projection, descriptor
occurrence, and current verified holders (§18.8), then streams and SHA-512
verifies the bytes before returning (§18.9 `fetch`). `holders` asserts the
plan's verified holders and their `$serve` order (store ids). `bytes` asserts
the exact fetched result, written as the literal UTF-8 `content` that was
staged (both identified by the same `$sha512`). Non-ok outcomes:

- `denied` — the surface grants no blob fetch (metadata-only projection, or the
  caller's authorization no longer admits the descriptor occurrence), §18.8.
- otherwise per the case (e.g. no verified holder can deliver a hash-clean
  result; see `red/all-holders-corrupt-fetch-outcome-unspecified`).

### `connector_set`

```hjson
{ connector_set: { connector: "fs-a", available: false } }
{ connector_set: { connector: "fs-a", fail: ["upload", "copy"] } }
{ connector_set: { connector: "fs-a", corrupt: "/stores['primary']" } }
```

Reconfigures a simulated connector from this step onward, modelling the
temporary connector failures of §18.12 and the corrupt store objects of §18.9:

- `available: false` — every connector operation fails deterministically.
- `fail: [...]` — the listed operations (`upload`, `download`, `copy`,
  `delete`) fail; others succeed.
- `corrupt: <store-view>` — the physical object currently held for the blob
  under test in the store selected by the view is replaced by a byte sequence
  whose SHA-512 no longer matches the descriptor (a tampered/bit-rotted object).
  It is observed as `corrupt` on the next verification (§18.5 states, §18.9).

Failures are clean: a rejected/delayed operation leaves committed application
state unchanged (§18.12) and produces no partial verified copy.

### `run_reconciler`

```hjson
{ run_reconciler: {}, expect: { outcome: ok } }
```

Runs the background reconciler (§18.6) to convergence: choose a verified
source, copy through connectors, verify hash+size at the destination, record it
verified, demote corrupt copies and repair from a verified holder, and drain
surplus after retention permits. Reconciler steps are actorless system
transitions using the ordinary type/transition checks (§18.6). The step
completes when no further convergence action is possible; a watch opened in a
later step observes the resulting placement state.

## Corpus readings (shape assumptions)

- Logical placement observations (`blob.$stored`, `blob.$satisfied`,
  `blob.$surplus`, `blob.$placement`, `blob.$policy`) are read through ordinary
  package views (§18.5 exposes them as engine-recorded logical observations).
  `$stored` and `$surplus` are sets of store rows; cases project them to `{ id }`
  and match them as `$unordered` sets, since §18.5 does not pin a container
  order. `$satisfied` is a bool. Placement-row *state* text
  (`pending|copying|verified|corrupt|draining`, §18.5) is read through
  `blob.$placement[store]` only where a case asserts a specific state.
- `watch` steps may carry `args` for parameterized surface views, mirroring
  `call`'s `args`; `expect_init` matches the initial result.
- Store-view / placement grammar in `$blob_storage` is written exactly as the
  §18.4 examples (`/stores['id']`, `/stores[:s | s.enabled]`, `$all`, `$any`,
  `$copies`/`$of`).
- Media-type declarations and descriptor `$media` values are canonical media
  types (§18.1, §18.2); the corpus never relies on wildcard forms (absent from
  the core, §18.2).

## Reused §19 steps

`red/artifact-blob-inclusion-selection-unspecified` reuses the `export` and
`inspect_artifact` steps documented in `tests/19-history-artifacts/NOTES.md`
(their semantics are unchanged here) to probe a §18/§19 boundary ambiguity.

## Other conventions

- All non-`ok` step outcomes carry `violates`, except `unspecified`, which
  carries no `violates` (per `tests/FORMAT.md`) and instead names the
  interacting rules whose gap leaves the behavior unpinned in a `note`/`detail`.
- A malformed *declared descriptor* (bad hex case, negative byte count) is a
  hostile client payload placed in `claim`, not malformed case syntax (FORMAT.md
  rule 5).
