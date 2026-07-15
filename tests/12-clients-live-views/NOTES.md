# §12 Clients and live views — chapter notes

Corpus extensions used by this chapter's cases, per FORMAT.md ("A chapter may
need an action this vocabulary lacks. Use a new, descriptive step key, and
document its semantics in `tests/<chapter>/NOTES.md`.").

## New step keys

### `authenticate` (standalone)

```hjson
{ authenticate: { role: "member", auth: "token", credential: "token:alice" },
  on: "c1", as: "alice" }
```

FORMAT.md only allows `authenticate` inside `connect`. The standalone form
adds an **additional** authentication context to an already-open connection,
per §11.8 (one network connection MAY multiplex subscriptions from several
sessions). `as` names the context for later `context:` references. The step
performs no external request by itself: authenticator selection and
verification are explicit at every external authenticated request
(§11.3–§11.4), so authentication failures surface on the requests and
subscriptions that use the context, not on this step.

### `manifest`

```hjson
{ manifest: {}, on: "c1", expect: { outcome: ok, surfaces: ["member.tasks"] } }
```

Performs the §12.1 `manifest` client operation for the connection's selected
authentication context (`context:` selects one on multiplexed connections).
`expect.surfaces` is the harness-normalized **complete** set of available
surface addresses, as a list of strings sorted by address: public surfaces as
`public.<name>`, root-level role surfaces as `<role>.<surface>`. §12.1 also
promises parameter and response shapes in the manifest; their wire encoding
is not specified by SPEC.md, so cases assert the surface set only.

### `operation_status`

```hjson
{ operation_status: { id: "op-..." }, on: "c1",
  expect: { outcome: ok, status: committed, frontier: "$any", commit: "$any" } }
```

Performs the §12.3 `operation` client operation. `status` is one of
`pending | committed | unchanged | rejected | unknown`; `frontier`, `commit`,
and `diagnostics` are matched when the status carries them.

### `resume`

```hjson
{ resume: { surface: "public.tasks", from: "$ref:f1", id: "w2" }, on: "c2",
  expect_result: { value: [ ... ] } }
```

Opens a subscription resuming from a retained frontier (§12.2). Per the spec,
the runtime may answer with the later authorized patches **or** a fresh
`init`; both are conforming, so `expect_result.value` matches the
reconstructed client result after the resume completes, whichever form was
used. A resume step may instead carry `expect:` with a non-ok outcome:
`denied` means the runtime either refused the subscription or closed it
before delivering any row data (the pinned property is that no unauthorized
data flows); `unspecified` documents a spec gap as usual.

### `expect_close`

Canonical registry step owned by this chapter (see the **Extended step
registry** in `tests/FORMAT.md`); other chapters reference the registry.

```hjson
{ expect_close: { watch: "w1", reason: "$any" } }
```

Asserts that the named subscription received `close(frontier, reason)`
(§12.2). `reason` contents are opaque; `"$any"` is recommended.

## Extended members on FORMAT.md steps

- **`watch.window`** — a §12.2 bounded-window request:
  `window: { $size: n, $anchor?: key | "$first" | "$last", $slide?: true }`.
  For a windowed subscription, `expect_init.value` and `expect_view.value`
  match the windowed client result.
- **`watch.expect`** (in place of `expect_init`) — asserts that opening the
  subscription fails. Convention: `denied` for authentication/authorization
  failures; `error` for malformed subscription parameters (e.g. an anchor
  identifying zero current occurrences, violating §12.2's "MUST identify
  exactly one current occurrence").
- **`watch.expect_init.frontier`** — matcher/binder for the opaque frontier
  delivered with `init` (commonly `"$bind:f1"` or `"$any"`).
- **`context`** on `watch` / `call` / `manifest` / `resume` — selects one of
  several authentication contexts on a multiplexed connection (see
  standalone `authenticate`). Defaults to the connection's only context.
- **`call.operation_id`** — attaches the §12.3 operation identifier to the
  call. Canonical registry member owned by this chapter (see the **Extended
  step registry** in `tests/FORMAT.md`); spelled `operation_id`, never `op_id`.
- **`call.abort_delivery: true`** — the transport exchange is dropped after
  the request is submitted and before any response is delivered. The step
  carries no `expect`: per §12.3 the outcome is unknowable to that client
  (the runtime MAY have canceled the request before admission, or committed
  it).
- **`expect.completion`** — `committed | unchanged`; asserts which §12.3
  completion a successful (`outcome: ok`) response reported. Canonical registry
  member owned by this chapter (see the **Extended step registry** in
  `tests/FORMAT.md`); distinct from the `operation_status` step's `status`.
- **`expect_one_of` alongside `expect_view`** — the view assertion admits
  several spec-allowed results (used under `concurrently`, where several
  serializations are valid).
- An omitted `value` in a step `expect` means "not asserted"; it does not
  mean the response must be empty.

## `hosts` conventions

FORMAT.md declares `hosts` as "optional simulated host components" without a
shape. This chapter uses:

- **`hosts.namespaces.<name>`** — a simulated host namespace satisfying the
  package's `$requires` entry (§16). `contract` is the required contract id;
  `functions.<fn>` documents effect class, typed signature, and deterministic
  behavior. Auth cases use `authsim` (`test.authsim@1`):
  `verify(credential: text) -> { auth: text, account: text }` splits the
  credential at the first `:`; any credential containing `:` verifies. This
  keeps authentication deterministic while exercising the real §11
  authenticator pipeline (`$verify`, `$actor`, `$check`,
  `$proof.auth == $auth_name` binding).
- **`hosts.operation_records.retention: "unbounded"`** — pins the §12.3
  "host policy" for operation-record expiry so `operation_status` assertions
  are deterministic. Corpus-wide assumption: the simulated host never expires
  operation records mid-case.

## Observed spec gaps

Captured as `outcome: unspecified` cases:

- `red/resume-with-foreign-frontier.hjson` — behavior when a resume presents
  a frontier issued for a **different** subscription stream is not pinned
  (§12.2 makes frontiers opaque and per-stream but mandates nothing for
  foreign tokens).
- `red/unknown-parameter-member.hjson` — whether an undeclared call-argument
  member fails §12.1 step-3 parsing or is ignored is not stated; this also
  makes §12.3's "equivalent request" comparison ill-defined for
  ignored-but-present members.

Noted inside case comments (outcome still pinned):

- `common/operation-id-duplicate-retry-executes-once.hjson` — the response
  **body** of a deduplicated retry is not clearly pinned ("delivered once for
  each successfully completed transport exchange" reads as per-exchange
  redelivery, but is arguable); the case asserts acceptance and at-most-once
  execution only.
- `common/manifest-lists-granted-surfaces.hjson` — manifest shape encoding
  (parameter and response shapes) is unspecified; only the surface set is
  asserted.
