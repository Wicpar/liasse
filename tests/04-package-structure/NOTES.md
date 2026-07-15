# §4 Package structure — chapter notes

Cases in this chapter cover §4 (Package structure) plus the normative
conventions it leans on: §2.5 (names, reserved `$`-names, unknown members,
package-name grammar), §9.2 (load pipeline), §16.2 (`$requires` resolution),
§19.5 (artifact entry structure, manifest), Annex C.1 (package grammar),
Annex D (canonical identity and integrity), Annex E.1 (versions).

## Extension steps

FORMAT.md has no vocabulary for building, tampering with, and loading a
`.liasse` archive, which §4.1/§4.2 are about. This chapter defines three
extension steps. They only appear in `suite: scenario` cases.

### `build_artifact`

```hjson
{ build_artifact: { as: "a1", files: { "<archive-path>": "<utf-8 content>" } } }
```

Builds the case's `package` (the `root` package in multi-package cases) into
a `.liasse` artifact exactly as a conforming builder would (§4.2 build,
§19.5 entry structure, Annex D canonical forms), then writes each `files`
entry into the archive at the given path, adding it or replacing any
same-named entry the builder produced. Binds the artifact as `as` for later
steps.

- `files` values are exact UTF-8 bytes. No trailing newline is implied.
- `manifest.json` entry checksums are computed over the **final** entry
  bytes, i.e. after `files` are applied. The build step performs **no
  validation** of the definition against the supplied bytes — the validation
  under test happens at load. This lets a case declare a `$sha256` that does
  not match the supplied bytes and observe the load-time rejection in
  isolation (manifest checksums match; only the definition-declared digest
  is wrong).
- **Implicit-load deferral:** when a scenario's first step is
  `build_artifact`, the case package is *not* implicitly instantiated on
  load (the FORMAT.md implicit `outcome: ok`); it is only instantiated
  through `load_artifact`. This is required because such packages may
  declare `$resources` whose bytes exist only inside the built artifact.

### `repack_artifact`

This chapter's byte-surgery step operates **in place** on one built artifact
label and can rebuild the ZIP container (`repack`) — a workflow the canonical
`tamper_artifact` (derive-a-new-label, owned by §19; see the **Extended step
registry** in `tests/FORMAT.md`) does not cover. It therefore has its own key,
`repack_artifact`.

```hjson
{ repack_artifact: {
    artifact: "a1"
    set: { "<path>": "<content>" }        // replace bytes of an existing entry
    add: { "<path>": "<content>" }        // append a NEW entry, even when the
                                          // name duplicates an existing entry
    remove: [ "<path>" ]
    merge_json: { path: "<path>", set: { "<member>": <value> } }
                                          // parse entry as JSON, set/replace the
                                          // given top-level members, re-serialize
                                          // in canonical member order
    repack: { order: "reverse", compression: "store" | "deflate",
              timestamps: "<iso>", format: "zip" | "zip64" }
                                          // rebuild the container with
                                          // byte-identical entry contents; only
                                          // container metadata changes
    rehash: true                          // AFTER the ops above, recompute every
                                          // manifest.json `entries` /
                                          // `included_modules` checksum from the
                                          // final entry bytes
} }
```

Byte-level archive surgery after build. It never updates `manifest.json`
checksums unless `rehash: true` is given. Without `rehash`, tampering
isolates Annex D checksum verification; with `rehash`, it models an attacker
who fixes the checksums, isolating content rules from integrity rules.

### `load_artifact`

```hjson
{ load_artifact: { from: "a1", expect: { outcome: ok } } }
```

Runs the host `create` lifecycle operation (§9.2) on the artifact.
Outcome mapping per §9.4: `ok` = `committed`; `invalid` = `rejected` with
validation diagnostics. Non-`ok` expectations carry `violates` as usual.

## Digests used by resource cases

```text
sha256("<h1>Invoice</h1>")  = 2163bddf11b2e116332448d10374d504f9508c148b72e5c9d970b88665330490
sha256("logo-bytes-v1")     = 75db1b791451743ece53a6f2595caadb42751c84ea9829e58b0550ea539f090a
```

## Deliberately not covered in this chapter's suites

- **§4.3 instance-incarnation lifecycle** (fresh incarnations on create,
  preserved on restore/rekey/remount, matched by reconciliation): requires
  §19 export/import/reconcile machinery and an incarnation-observation
  oracle; belongs to the `#history` chapter corpus.
- **§4.1 `state/`, `history/`, `blobs/`, `modules/` entry semantics**:
  §4.1 explicitly delegates them to §19; covered there.
- **Module-only members (`$config`, `$use`, `$deps`, `$expose`) positive
  semantics**: exercising them requires module mounting (§13). Only their
  rejection on *application* definitions is covered here.
- **§4.2 "runtime forms have no portable identity" / MAY
  normalize-compile-discard**: implementation freedom with no observable;
  untestable by construction.
- **§4.2 definition-identity equality across authoring variants** (comments,
  Hjson conveniences, member order produce the same canonical identity):
  the step vocabulary has no spec-defined identity-comparison oracle, and
  inventing one would encode implementation behavior.
- **§4.4 behavioral consequences of selected semantics** (e.g. the numeric
  result of `decimal_division` modes): the observable results are defined by
  Annex A and §5–§6 and belong to those chapters. §4 coverage here is limited
  to accepting and rejecting the declarations.
