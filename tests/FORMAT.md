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
                                     // EXCEPT unspecified (see policy below)
  detail: "field references a collection that does not exist"
}
```

**`unspecified` carries no `violates`.** Every other non-`ok` outcome names the
violated rule in `violates`; `unspecified` asserts that *no* rule pins the
behavior, so there is nothing to cite. An `unspecified` outcome MUST instead
carry an explanation of the gap — a case-level `note`, or a `detail` on the
`unspecified` `expect` block — naming the interacting rules whose combination
leaves the behavior unpinned.

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
| `unspecified` | the spec does not pin the behavior — carries **no** `violates`; `note`/`detail` must explain the gap |

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
  `violates`, except `unspecified` (no `violates`; explained by `note`/`detail`).
- A chapter may need an action this vocabulary lacks. Use a new, descriptive
  step key, and document its semantics in `tests/<chapter>/NOTES.md`. The
  harness treats undocumented step keys as corpus errors. Steps reused by three
  or more chapters are promoted to the **Extended step registry** below; a
  chapter that uses a registry step references the registry instead of
  redefining it, and `tests/<chapter>/NOTES.md` documents only steps local to
  that chapter.

## Extended step registry

Steps and step-members reused across three or more chapters. Each has a single
canonical name, schema, and owning chapter; other chapters reference this
registry rather than redefining the step. Chapter `NOTES.md` files keep only
their chapter-local extensions.

### `host_load` — owning chapter §9/§16

```hjson
{ host_load: { package: "v2" }, expect: { outcome: invalid, violates: [...] } }
```

Applies the §9.2 host lifecycle load of a definition (an inline package object
or a label from the case's `packages` map) against the active root instance
(load / update path). Outcome mapping per §9.4: `ok` = committed;
`invalid` = the §9.4 `rejected` load outcome (validation failed, prior
application remains active). Distinct from `load_artifact` (§04), whose input
is a prebuilt `.liasse` artifact rather than a definition.

### `module_install` — owning chapter §13

```hjson
{ module_install: { space: "<display path of the module space>",
    request: { $name, $module, $config?, $data?, $use? } },
  expect: { outcome: ok } }
```

Performs the §13.3 `modules.install` into the named module space. `$module`
names a package `name@version`, resolved against the case's `packages` map by
each entry's declared `$module` value. `$use` bindings are display paths of
sibling instances (§13.3). A successful install is a composition change and
therefore a commit (§2.4).

### `tamper_artifact` — owning chapter §19

```hjson
{ tamper_artifact: { from: "a1", as: "a1x", ops: [ ... ] } }
```

Derives a **new** labeled artifact by applying deterministic edits to a copy;
the source label is left untouched. Op vocabulary (applied in order):
`corrupt_entry`, `set_entry`, `remove_entry`, `duplicate_entry`, `add_entry`,
`copy_entry_from`, `edit_json`, `duplicate_json_member`, `edit_cbor`,
`rewrite_identifier`, `fix_checksums`, `add_manifest_entry`. Full op semantics
are in `tests/19-history-artifacts/NOTES.md`. (§04's byte-surgery-with-repack
step, which mutates one label in place, is the distinct chapter-local
`repack_artifact`.)

### `operation_id` — member on `call` / `blob_put`, owning chapter §12

```hjson
{ call: "public.tasks.add", args: { title: "x" }, operation_id: "op-7",
  expect: { outcome: ok, "...": true } }
```

Attaches the §12.3 / §D.8 external high-entropy operation identifier. Two
submissions carrying the **same** `operation_id`, the same target surface, the
same selected authenticator, and an equivalent request model are one operation
(a retry); §12.3 requires at-most-once execution for that pair. A call with no
`operation_id` is a new operation on every submission. (Spelled
`operation_id`, never `op_id`.)

### `expect.completion` — member on a `call`/mutation `expect`, owning chapter §12

```hjson
{ call: "public.docs.tag", args: { ... }, expect: { outcome: ok, completion: unchanged } }
```

`completion` is `committed | unchanged`; it asserts which §12.3 / §8.9 success
completion an `outcome: ok` response reported. Distinct from the
`operation_status` step's `status` member (`pending | committed | unchanged |
rejected | unknown`), which reports a queried operation record (§12.3).

### `expect_close` — owning chapter §12

```hjson
{ expect_close: { watch: "w1", reason: "$any" } }
```

Asserts that the named subscription received `close(frontier, reason)` (§12.2)
once all prior commits on its connection are reflected. `reason` contents are
opaque; `"$any"` is recommended.

### `expect_one_of` inside `expect_init` — owning chapter §12/§23

```hjson
{ watch: "public.notes", id: "w1", expect_init: { expect_one_of: [ [...], [...] ] } }
```

Like `expect_init`, but the initial value matches any one of the listed
spec-allowed results. Used after a `concurrently` race where the spec admits
several serializations, mirroring FORMAT.md's `expect_one_of` on `expect_view`.
(This is the single canonical form; the earlier `expect_init_one_of` spelling
is retired.)

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

## Style and wire conventions

These are corpus-wide conventions the harness does not enforce but authors keep
consistent, so simple tooling and grep work across every file.

- **Bare outcome tokens.** Write outcome and completion/status values unquoted:
  `outcome: ok`, `completion: unchanged`, not `outcome: "ok"`.
- **Bare package member keys.** Use Hjson quoteless keys for simple
  identifiers and `$`-names (`$app:`, `$model:`, `$key:`, `n:`), not
  JSON-style quoted member names (`"$app":`). Quote a key only when it is not a
  simple identifier — display paths, media/signature keys, UUIDs, numeric
  strings, and the `"..."` extra-members matcher stay quoted.
- **`operation_id`, not `op_id`**, for the §12.3 operation identifier.
- **Spec anchors: chapter anchor before section.** Within a `spec:`/`violates:`
  array, a chapter `#anchor` precedes the `§section` ref(s) it introduces
  (`["#history", "§19.7"]`); for several rules the array pairs each `#anchor`
  with its own section(s). Never lead with a `§section` ahead of its anchor.
- **`int` wire values are JSON strings.** Annex A.1 pins the canonical
  strict-JSON value of `int` as a "JSON string with canonical base-10 digits".
  An `int` value in `args`, `$data` seeds, and asserted results is therefore
  the quoted string `"20"`, never the bare number `20`. (`decimal`, `uuid`,
  `date`, `timestamp`, `duration` are likewise JSON strings per Annex A.1;
  `bool` and `json` numbers/booleans stay bare.)

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
