# Chapter 9 (Package loading and bootstrapping) — corpus notes

Cases in this directory exercise SPEC.md §9 (`#loading`): seed data (§9.1), host
lifecycle operations (§9.2), bootstrap atomicity (§9.3), and load outcomes
(§9.4), together with the load-time rules §9.2 incorporates from
`#package-structure`, `#state-model`, `#host-namespaces`, `#evolution`,
`#conformance`, and `#annex-d`.

## Extension step: `host_load`

FORMAT.md has no step for the §9.2 host lifecycle operation
`load(target, artifact)`. This chapter defines:

```hjson
{ host_load: { package: { ...definition... } },
  expect: { outcome: ok, result: committed } }
```

Semantics:

- The harness builds a `.liasse` artifact from the inline definition using the
  same pipeline as the case-level `package`, then invokes the host `load`
  operation against the case's root package instance (§9.2).
- `host_load` is a host operation: it runs outside any client connection and
  carries no actor, role, or credential (§9.3).
- On `expect.outcome: ok`, the additional member `result` states the §9.4
  lifecycle outcome and MUST be `committed` or `unchanged`.
- A §9.4 `rejected` result is expressed with the standard non-ok outcome
  vocabulary plus `violates` (see the outcome mapping below). After a non-ok
  `host_load`, the prior active composition must still be in force (§9.2,
  §9.4); subsequent steps assert against it.

## Extension case member: `resources`

To exercise artifact-entry and digest verification (§4.1, §9.2 step 2) a case
may declare archive entries next to `package`:

```hjson
resources: { "resources/logo.txt": "utf-8 entry content" }
```

The harness stores each value as the exact UTF-8 bytes of the entry at the
given archive path when building the `.liasse` artifact. Entries are only ever
referenced from the same case's `$resources` declarations.

This is the same concept as §04's `build_artifact.files: { "<path>": "<bytes>" }`
(`tests/04-package-structure/NOTES.md`) — raw entry bytes at an archive path.
The two differ only in position: `resources` is a **case-level** member for
cases that rely on the implicit load, while `build_artifact.files` is a **step**
parameter for cases that build and load the artifact explicitly.

## Shape of the `hosts` member

FORMAT.md declares `hosts` ("optional simulated host components") without a
shape. Cases in this chapter use:

```hjson
hosts: {
  namespaces: {
    "<registered-name>": "<contract-id>@<version>"   // e.g. "password": "liasse.password@1.2.0"
  }
}
```

`namespaces` simulates the Rust context registrations of §16.4: the key is the
registered expression-namespace name, the value the implemented semantic
contract identity and version (§16.2). The core namespaces of §16.1 are always
available and never listed. `hosts: { namespaces: {} }` means no non-core
namespace is registered. Omitting `hosts` entirely leaves the default harness
context (core only) — cases that depend on registration state declare `hosts`
explicitly.

## Outcome mapping at load time

§9.4 gives lifecycle operations three results (`committed`, `unchanged`,
`rejected`). FORMAT.md's finer outcome vocabulary is mapped as follows,
consistently across this chapter:

- `invalid` — the definition itself is statically bad: syntax, typing, unknown
  members, unsupported `$liasse`, unresolved or incompatible requirements,
  resource path/digest failures (§9.2 steps 1–6).
- `rejected` — the definition is well-formed but the seeded or prospective
  state violates admission-class constraints: key agreement, checks, refs,
  uniqueness (§9.1, §9.2 steps 7–8). These are the same rule classes FORMAT.md
  lists under `rejected` for mutation admission, and §9.1 mandates that seed
  rows pass through exactly those rules.

Both map to the single §9.4 `rejected` lifecycle result; the split only
follows FORMAT.md's vocabulary. Where a case's classification between the two
could be argued, its `note` says so; the non-ok outcome itself is never in
doubt in those cases.

## Wire-value reminders used by expectations

- `int` canonical wire value is a JSON *string* of base-10 digits (Annex A.1),
  so an expected count of two is written `"2"`.
- An absent optional value (`none`) is an omitted member on the wire
  (Annex A.1); expectations use `"$absent"`.
- Seed map member names use canonical encoded key text (§9.1, Annex D.2).
