# §23 Rust host and implementation contract — chapter notes

Cases in this chapter exercise SPEC.md [§23 Rust host and implementation
contract](../../SPEC.md#rust-host) and the conformance-class sentence it rests
on (§2.1: "A host component conforms to the typed contract it registers"; "a
package requiring an unavailable or incompatible component fails validation
before activation"). §23 is a synthesis chapter: its normative force is in a
handful of cross-cutting sentences that pull together the detailed component
contracts of §16 (namespaces), §17 (keyrings/providers), §18 (connectors), and
the runtime rules of §9/§12/§22. Every case anchors on a §23 sentence and cites
the concrete component/runtime rule it composes with.

The §23 sentences under test:

- §2.1 — a package requiring an **unavailable or incompatible component fails
  validation before activation**; a runtime/host component conforms to the
  typed contract it registers.
- §23.3 — every implementation choice **MUST preserve declared identity, values,
  order, atomicity, admission, history, live-view, and completion behavior**.
- §23.5 — **trusted host access** bypasses external role authentication while
  retaining the package's type rules, module boundaries, refs, deletion
  planning, constraints, meters, provider contracts, serial admission, and
  atomicity; an operator transition that changes the active application creates
  an **ordinary commit with host provenance recorded separately from the
  application actor field**.
- §23.6 — **budget exhaustion produces operational backpressure or a rejected
  request according to the declared API contract; it never permits a partial
  state transition**; a runtime MAY map modules/workloads to isolated processes
  while preserving one logical application state and serial admission order.
- §23.8 — external argument values and credentials remain call-local; runtime
  errors preserve their structured category independently of backend details.

## Simulated host components (`hosts`)

FORMAT.md leaves the `hosts` shape to chapters. This chapter reuses the shapes
already established by sibling chapters, verbatim, and cites them:

- **`hosts.namespaces`** — as a *list* of descriptors, per
  `tests/16-host-namespaces/NOTES.md` (id, version, `interface_hash`, `types`,
  `functions` with `signature`/`effect`/`op`). Used by the off-contract-type
  and impure-pure cases. This chapter adds two `op` behaviors (below).
- **`hosts.namespaces`** — as a *map* `{ <name>: "<contract>@<major>" }`, per
  `tests/17-keyrings/NOTES.md`, when a case needs the simulated `cose` or
  `token` namespace rather than a bespoke function. A case uses only one of the
  two `hosts.namespaces` forms; they are never mixed in one file.
- **`hosts.key_providers`** — per `tests/17-keyrings/NOTES.md` (`algorithms`,
  `operations`, `generate`, `bind`, `protection`, `external_keys`). The
  simulated `cose` namespace signs through a keyring's active version (§17.7).
- **`hosts.connectors`** — per `tests/18-blobs/NOTES.md` (`capabilities`,
  `available`).
- **`hosts.token`** — the `test.token@1` verifier namespace per
  `tests/11-auth-sessions/NOTES.md`, for authenticated-client cases.

### Added `op` behaviors (namespace list form, extends §16 NOTES table)

| op                | effect | behavior                                                                                          |
|-------------------|--------|---------------------------------------------------------------------------------------------------|
| `off_type`        | pure   | the registered descriptor pins signature `(int) -> int`, but the component **returns a `text`** — a value that violates the contract it registered (§2.1). Models a nonconforming host component. |
| `drifting`        | pure   | declared effect `pure` (§16.3: "same logical inputs produce the same output"), but the component **returns a different value on each evaluation** for identical inputs — a component that lies about its effect class. |

Both model a component that violates the typed contract of §2.1/§16.3. The
spec assumes registered components conform; it does not pin runtime handling of
a component that does not. Cases using these ops are therefore
`outcome: unspecified`.

## Extension steps

### `operator`

```hjson
{ operator: { call: "<model $mut path>", args: { ... } },
  expect: { outcome: ok, value: { ... } } }
```

A **trusted host operator transition** (§23.5; the "custom trusted operations"
of §23.5, driven through the host lifecycle path of §9.2, not an application
role). `call` names a mutation declared in `$model` by dotted path from the
root (`"add_task"` for a root `$mut`; `"tasks.<key>.complete"` for a
collection-row `$mut`). The operator invokes it **directly, bypassing external
role authentication**, while the runtime still enforces the package's type
rules, refs, deletion planning, constraints, meters, provider contracts, serial
admission, and atomicity (§23.5). The transition runs with **no `$actor`**
(§22.8: engine maintenance runs without an actor; §23.5: host provenance is
recorded separately from the application actor field). A successful operator
transition creates an **ordinary commit** (§23.5) that advances live views like
any other (§22.6, §12.2). `expect` uses the standard outcome vocabulary:
`rejected` for an admission-time failure (check/ref/meter/uniqueness), `ok`
otherwise. Non-`ok` step outcomes carry `violates`.

### `budget_set`

```hjson
{ budget_set: { mutation_time: "PT1S" } }   // a finite mutation-time budget (§23.6)
```

Installs a host **resource budget** (§23.6) from this step onward. Only
`mutation_time` (one of the §23.6 budgets: "query and mutation time") is used
by this chapter. The harness treats a provider operation configured to `hang`
(see `provider_set` below) under **any finite** `mutation_time` budget as
deterministically exhausting that budget for the enclosing mutation: a
never-returning operation necessarily exceeds any finite mutation-time budget.
Per §23.6, exhaustion yields backpressure **or** a rejected request per the
declared API contract and **never permits a partial state transition**. The
CLASS of surfaced outcome (backpressure vs rejection) is not pinned by SPEC.md
for a package that declares no such policy, so no case asserts `rejected`
outright: `common/budget-exhaustion-never-partial-transition` marks the
exhausted operator step `outcome: unspecified` and asserts only the no-partial
MUST (a trailing empty watch), and `red/budget-backpressure-or-reject-choice-
unspecified` documents the class gap directly.

### `provider_set` (reused from §17, one addition)

Reused verbatim from `tests/17-keyrings/NOTES.md`
(`{ provider_set: { provider, fail: [...], available: bool } }`), plus two
additions:

```hjson
{ provider_set: { provider: "test-kp", hang: ["sign"] } }
{ provider_set: { provider: "test-kp", invalid_public_key: ["generate"] } }
```

`hang: [...]` — the listed provider operations **never return** (they neither
succeed nor fail). Models a misbehaving/unresponsive registered host component.
With a finite `mutation_time` budget in force the enclosing mutation is
resolved by budget exhaustion (§23.6); with no budget in force the outcome is
not pinned by the spec (see `red/no-time-budget-hanging-provider-unspecified`).

`invalid_public_key: [...]` — the listed operations (`generate`, `bind`)
**return** a structurally invalid / wrong-type public key + metadata rather than
failing. Models a component that lies with a garbage value. §17.4 step 2 ("read
and validate its public key and provider metadata") rejects the replacement, so
§17.9 keeps the current version active. Distinct from `fail`, where the provider
cleanly errors: `invalid_public_key` exercises the wrong-type-result path of the
§2.1 registered-component contract at rotation time.

### `connector_set` / `blob_put` / `blob_get` (reused from §18, one addition)

Reused verbatim from `tests/18-blobs/NOTES.md` for the connector-failure and
read-tampering cases, plus one addition:

```hjson
{ connector_set: { connector: "fs-a", tamper_download: true } }
```

`tamper_download: true` — from this step onward, byte reads served by that
connector return content whose SHA-512 does **not** match the descriptor (a
lying / compromised read transport), while the placement row is **not** marked
`corrupt` (this is a transport lie, not observed bit-rot). It models a
misbehaving registered connector on the fetch path. The only thing §18 pins here
is the **negative guarantee**: §18.9 verifies the hash "before delivering a
successful result" and §18.8 says a successful fetch returns "exactly the bytes
identified by `$sha512`", so the runtime MUST NOT deliver the tampered bytes as a
successful fetch. Whether the fetch then *recovers* from an honest holder is a
§18.8 client **MAY** ("A client MAY probe holders ... and replace ranges received
from a mismatching source"), **not** a MUST — so even with an honest verified
holder present the fetch outcome (ok + true bytes vs a failure) is not pinned.
`red/connector-tampered-read-refetched-from-verified-holder` therefore records
the fetch as `outcome: unspecified`, asserting only the no-tampered-success
invariant, consistent with the sibling
`tests/18-blobs/red/all-holders-corrupt-fetch-outcome-unspecified`. This differs
from §18's `corrupt` (which replaces the stored object, is observed `corrupt` on
next verification, demotes the copy, and triggers §18.6 reconciler repair — the
path under which `tests/18-blobs/red/corrupt-copy-demoted-and-repaired` may then
assert a firm `ok` fetch).

## Reused FORMAT.md / sibling steps

- `restart: {}`, `advance_time`, `concurrently: [...]`, `expect_one_of`,
  `expect_view`, `watch`/`expect_init` — FORMAT.md. An `operator` step (above)
  may appear inside a `concurrently` branch: it is an ordinary admitted request
  sharing the one serial order (§23.5, §22.3), used by
  `red/operator-application-race-either-serial-order`. A `watch`'s `expect_init`
  may carry an `expect_one_of` (the initial result matches one listed value) or,
  where the spec pins no value, `{ outcome: unspecified }` with an explanatory
  `note`/`detail` and no `violates` (per `tests/FORMAT.md`; used by
  `red/impure-pure-function-replay-divergence-unspecified`, whose post-replay
  recomputed value is not pinned).
- `connect` / `authenticate` with `{ role, auth, credential }`, and role-surface
  addresses `"<role>.<surface>[.<mut>]"` — per `tests/11-auth-sessions/NOTES.md`.
- `expect.completion: committed | unchanged` — per
  `tests/12-clients-live-views/NOTES.md` / `tests/22-runtime-semantics/NOTES.md`.

## Spec gaps captured as `outcome: unspecified`

- `red/operator-commit-actor-provenance-unspecified` — §23.5 records host
  provenance "separately from the application actor field" and §22.8 runs
  maintenance "without an actor", but the spec does not pin what a mutation that
  *reads* `$actor` observes under a host operator transition (parallels
  `tests/22-runtime-semantics/red/public-request-references-actor-unspecified`),
  nor whether the host provenance / empty actor field is application-readable.
- `red/namespace-returns-off-contract-type-unspecified` — §2.1/§16.2 assume a
  registered component conforms to its typed contract; runtime handling of a
  component that returns an off-contract value at evaluation time is not pinned.
- `red/impure-pure-function-replay-divergence-unspecified` — §16.3 says pure
  functions may be recomputed during replay; the spec does not pin what happens
  when a component declared `pure` in fact returns different values, so replay
  divergence is unspecified.
- `red/no-time-budget-hanging-provider-unspecified` — §23.6 makes budgets a host
  MAY; with no mutation-time budget in force, the spec does not mandate any
  timeout, so a hanging registered component's outcome is unspecified.
- `red/budget-backpressure-or-reject-choice-unspecified` — §23.6 admits *either*
  operational backpressure *or* a rejected request "according to the declared
  API contract"; when the package's API contract does not itself pin the choice,
  which of the two occurs is unspecified.
- `red/connector-tampered-read-refetched-from-verified-holder` — §18.9 forbids
  delivering tampered bytes as a successful fetch (the normative negative
  guarantee), but §18.8 makes recovery from another verified holder a client MAY,
  so whether a single fetch past a lying serve-preferred connector recovers (ok +
  true bytes) or fails is unspecified even when an honest holder exists.

## Known coverage gaps (see structured report)

- §23.5 "module boundaries" under host operator access: modules (§13) are heavy
  and owned by `tests/13-modules/`; no operator-across-module-boundary case is
  authored here.
- §23.5 "type rules" under host operator access: an operator transition writing
  an off-type value is retained by the same type constraints (§22.1) as an
  application mutation, but the outcome TOKEN for a mistyped request boundary is
  itself ambiguous between `invalid` (static, if caught as a malformed request)
  and `rejected` (admission), and static namespace/field type-mismatch rejection
  is already owned by `tests/16-host-namespaces/common/namespace-signature-type-
  mismatch-rejected` and the state-model type chapters. No operator type-rule
  case is added here to avoid encoding an under-determined outcome token; the
  wrong-type component contract is instead exercised at the component boundary
  by `red/namespace-returns-off-contract-type-unspecified` and
  `red/rotation-provider-invalid-public-key-keeps-current-active`.
- §23.8 "External argument values and credentials remain call-local": not
  externally observable — the diagnostic/audit record shape is not pinned by
  SPEC.md, so absence of a credential in a diagnostic cannot be asserted through
  any surface (same conclusion as `tests/11-auth-sessions/NOTES.md`).
- §23.7 PostgreSQL example and §23.4 context-construction Rust API are
  informative ("MAY", "Representative"): no observable semantics to assert.
</content>
