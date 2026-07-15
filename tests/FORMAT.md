# Liasse conformance corpus — case file format (format 1)

This directory is the file-based conformance test corpus for the Liasse v0.5
specification ([SPEC.md](../SPEC.md)). It is written **before** the
implementation and is implementation-agnostic: the `liasse-testkit` crate
executes these files against a runtime + store and reports per-case verdicts.
Implementation work makes cases pass; a case is only ever changed when it
contradicts SPEC.md.

## Layout

```
tests/
  FORMAT.md                  <- this file
  <chapter>/                 <- one directory per spec chapter or annex
    common/<case>.hjson      <- common-case suite
    red/<case>.hjson         <- red-agent (adversarial) suite
```

- One case per file. Filename is the case `name` plus `.hjson`, kebab-case.
- `common/` holds the normative happy paths and ordinary error paths every
  correct implementation must satisfy.
- `red/` holds adversarial, uncomfortable, hostile scenarios: boundary abuse,
  identity confusion, ordering races, privilege escalation, unicode edge
  cases, tampering, resource abuse. Red cases still assert spec-mandated
  outcomes — they are conformance cases with an attacker's mindset, not fuzz
  noise.

Files are Hjson (the spec's authoring form). Comments are allowed and
encouraged — a red case should explain its attack in a comment.

## Case shape

```hjson
{
  format: 1
  name: duplicate-key-field          // matches the filename
  suite: static                      // "static" or "scenario"
  spec: ["#state-model"]             // SPEC.md anchors (or "§5.4" refs) that
                                     // justify EVERY expectation in the case
  tags: []                           // optional: unicode, authz, ordering, ...
  note: "..."                        // optional prose; required for
                                     // outcome: unspecified

  package: { $liasse: 1, $app: "t.case@1.0.0", $model: { ... } }
  // Multi-package cases (modules, migrations) instead use:
  //   packages: { "<label>": { ...definition... }, ... }
  //   root: "<label>"

  hosts: { ... }                     // optional simulated host components
  expect: { ... }                    // static cases
  steps: [ ... ]                     // scenario cases
}
```

Every case is fully self-contained: definitions inline, no references to
files outside the case. Prefer the smallest package that isolates exactly the
rule under test.

### `suite: static`

The package is built and loaded; nothing is executed. `expect` states the
load outcome:

```hjson
expect: {
  outcome: invalid                   // see outcome vocabulary below
  violates: ["#refs"]                // spec anchor(s) of the violated rule —
                                     // required for every non-ok outcome
  detail: "field references a collection that does not exist"
}
```

### `suite: scenario`

The package loads successfully (an implicit `outcome: ok` on load), then
`steps` run in order. Each step either performs an action or asserts.

## Outcome vocabulary

| outcome       | meaning                                                        |
|---------------|----------------------------------------------------------------|
| `ok`          | accepted / succeeds                                            |
| `invalid`     | statically rejected at build/load/validation time              |
| `denied`      | rejected by authentication, roles, or permissions              |
| `rejected`    | admission-time rejection (checks, keys, refs, uniqueness, meters, limits) |
| `error`       | other runtime failure the spec mandates                        |
| `unspecified` | the spec does not pin the behavior — `note` must explain the gap |

`unspecified` cases are valuable: they document spec ambiguities discovered
while writing the corpus. The harness records them without judging.

## Steps

```hjson
{ connect: "c1", authenticate: { ... } }   // open a logical client connection;
                                           // authenticate uses whatever
                                           // mechanism the case's package
                                           // defines (§11)
{ disconnect: "c1" }

{ call: "public.tasks.add", args: { title: "x" }, on: "c1",
  expect: { outcome: ok, value: { id: "$bind:t1", title: "x" } } }

{ watch: "public.tasks", on: "c1", id: "w1",
  expect_init: { value: [ ... ] } }
{ unwatch: "w1" }
{ expect_view: { watch: "w1", value: [ ... ] } }   // value after all prior
                                                    // commits on c1's frontier
{ advance_time: "PT1H" }                  // virtual clock, ISO-8601 duration
{ restart: {} }                           // stop and replay the runtime;
                                          // durable state must survive
{ export: { as: "a1", ... } }             // artifact operations (§19)
{ import: { from: "a1", ... } }
{ reconcile: { from: "a1", ... } }
{ blob_put: { ... } } / { blob_get: { ... } }      // §18
{ concurrently: [ [ ...steps... ], [ ...steps... ] ] }
  // interleaving-unspecified admission race; per-branch expectations may use
  // expect_one_of: [ {...}, {...} ] when the spec admits several serializations
}
```

- `on` defaults to the single connection when only one exists; `connect` is
  implicit for single-client cases that never authenticate.
- Step `expect` uses the same outcome vocabulary; non-ok outcomes carry
  `violates`.
- A chapter may need an action this vocabulary lacks. Use a new, descriptive
  step key, and document its semantics in `tests/<chapter>/NOTES.md`. The
  harness treats undocumented step keys as corpus errors.

## Determinism and matchers

Cases never depend on wall-clock time or real randomness. The virtual clock
starts at `2026-01-01T00:00:00Z` and only `advance_time` moves it. Generated
values are matched, bound, and reused:

| matcher            | matches                                        |
|--------------------|------------------------------------------------|
| `"$any"`           | any value                                      |
| `"$any:uuid"`      | any well-formed uuid                           |
| `"$any:timestamp"` | any well-formed timestamp                      |
| `"$bind:NAME"`     | any value; binds it as NAME for later steps    |
| `"$ref:NAME"`      | exactly the value bound to NAME                |
| `"$absent"`        | the member must be absent                      |
| `{ $unordered: [...] }` | the array, ignoring order (sets)          |

`$ref:NAME` may also appear in `args` to send a previously bound value.
Expected objects match exactly: members not listed must not be present,
unless the object contains `"...": true`, which allows extra members.

## Rules for authors

1. **Externally deducible only.** Every expectation must follow from SPEC.md
   text alone; cite the anchors in `spec:`. Never invent behavior, never
   encode a guess about implementation behavior. If the spec is ambiguous,
   use `outcome: unspecified` and explain in `note`.
2. **No tautologies.** A case whose expected value could only be produced by
   running the program is worthless.
3. **No performance cases.** The corpus asserts semantics only.
4. **Isolate the rule.** One case, one rule (or one interaction of rules).
   Small packages; short step lists; the case name states the rule under test.
5. **Red means hostile, not invalid Hjson.** Red cases are well-formed case
   files describing hostile inputs and sequences — malformed *payloads*
   belong inside `package`/`args` values, not in the case file syntax itself.
