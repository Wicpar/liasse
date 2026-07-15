# §16 Host namespaces — chapter notes

Cases in this chapter simulate registered host namespaces and exercise the
`$requires` contract (SPEC.md `#host-namespaces`), descriptor resolution,
effect classes, and pinning. This file documents the shapes and extension
steps this chapter adds on top of tests/FORMAT.md.

## `hosts.namespaces` — simulated namespace descriptors

FORMAT.md allows `hosts: { ... }` for simulated host components but does not
define a shape for namespaces. This chapter uses:

```hjson
hosts: {
  namespaces: [            // a LIST: the context may register several
    {                      // descriptors, including conflicting ones (red)
      id: "test.util"      // semantic contract name (package-name grammar)
      version: "1.2.0"     // resolved descriptor version
      interface_hash: "ih-util-1"   // stands in for the semantic interface
                                    // hash of §16.2; equal string = equal
                                    // interface, different string = different
      types: {             // optional namespace-defined named value types
        opaque: { codec: "bytes", key_eligible: false }
      }
      functions: {
        double: { signature: "(int) -> int", effect: "pure", op: "double" }
      }
    }
  ]
}
```

- `id` + `version` are matched against `$requires` values (`name@major`
  identifies the semantic contract and compatible major, §16.2).
- `effect` is the §16.3 effect class: `pure`, `verifier`, or `generated`.
- `op` names one of the deterministic behaviors below so expected values are
  externally deducible from this file rather than from an implementation.

### Simulated function behaviors (`op`)

| op       | effect    | behavior                                                             |
|----------|-----------|----------------------------------------------------------------------|
| `double` | pure      | returns its single integer argument multiplied by 2                  |
| `token`  | generated | returns an opaque non-empty text, distinct for every admitted generated evaluation; cases only `$bind` and `$ref` it, never assert a literal |
| `accept` | verifier  | the descriptor carries `accepts: { "<credential>": <proof> }`; verification succeeds iff the credential text is a key of `accepts` and returns the mapped typed proof; any other credential yields a verification diagnostic (authentication fails) |

## Extension steps

### `load`

```hjson
{ load: { package: "v2" }, expect: { outcome: invalid, violates: [...] } }
```

Applies the host lifecycle operation `load(target, artifact)` of §9.2 to the
root package instance, using the case's `packages` entry named by `package`.
`expect.outcome: ok` maps to §9.4 `committed`/`unchanged`; `invalid` maps to
§9.4 `rejected` (validation failed; the prior application remains active).

### `reopen`

```hjson
{ reopen: { hosts: { ... } }, expect: { outcome: invalid, violates: [...] } }
```

Stops the runtime, replaces the simulated host components with the given
`hosts` object (same shape as the case-level `hosts`), and performs the host
`open(store)` operation of §9.2 against the durable store. `expect.outcome:
ok` means the recorded active composition validates and the application is
available again; `invalid` means `open` produced a diagnostic, the
application does not become available, and durable state is left unchanged.
This differs from `restart` only in that the host context is replaced.

## Addressing assumptions

- Role-scoped surfaces are addressed `"<role>.<surface>"` on an
  authenticated connection, mirroring FORMAT.md's `"public.<surface>"` form.
- The `authenticate` payload mirrors the §11.4 request members:
  `{ role, auth, credential }`.
- `connect`/`authenticate` steps accept an `expect` with the ordinary outcome
  vocabulary (`denied` for authentication failure), as FORMAT.md already
  allows for steps generally.

## Known gaps deliberately not turned into cases

- Whether an implementation may deterministically prefer the newest of
  several *distinct compatible versions* of one contract (e.g. 1.2.0 and
  1.3.0 both registered) is not pinned by §16.2; the ambiguity case in
  `red/ambiguous-descriptor-resolution-rejected` therefore uses two
  descriptors with the *same* id and version but different interface hashes,
  which no reading can resolve.
- The exact function rosters of the core namespaces (`hex`, `base64`, `sha`,
  `string`, `convert`, `time`) are not enumerated anywhere in SPEC.md; core
  cases only use function names attested in spec examples.
