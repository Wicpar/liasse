# §17 Keyrings and key providers — chapter notes

## Extension steps

FORMAT.md lacks actions for host-driven key lifecycle and provider fault
injection. This chapter adds two step keys.

### `keyring_admin`

```hjson
{ keyring_admin: { ring: "/session_keys", op: "bind_activate", external: "ext1" },
  expect: { outcome: ok } }
{ keyring_admin: { ring: "/session_keys", op: "revoke",  version: "$ref:v1" }, expect: { ... } }
{ keyring_admin: { ring: "/session_keys", op: "destroy", version: "$ref:v1" }, expect: { ... } }
```

A trusted host operator action (SPEC §23.5) driving the explicit logical
key-lifecycle transitions of §17.3, §17.4 and §17.9:

- `bind_activate` — the manual policy of §17.4: bind the externally created
  provider handle named by `external` (see `hosts.key_providers.*.external_keys`
  below), validate its public metadata, and activate it through the same
  transition as automatic rotation (atomically retiring any prior active
  version).
- `revoke` / `destroy` — the explicit revocation / destruction transitions of
  §17.3 and §17.9, addressed by a previously bound version `id`.

The step completes only after its transition is admitted; a watch opened in a
later step observes it. `expect` uses the standard outcome vocabulary.

### `provider_set`

```hjson
{ provider_set: { provider: "test-kp", fail: ["sign"] } }
{ provider_set: { provider: "test-kp", available: false } }
```

Reconfigures the simulated key provider from this step onward: operations
listed in `fail` fail deterministically; `available: false` fails every
provider operation. Models the provider failures of §17.9. Failures are clean
rejections with no partial provider effects.

## Simulated host components (`hosts`)

FORMAT.md leaves the `hosts` shape to chapters. This chapter uses:

```hjson
hosts: {
  namespaces: { cose: "liasse.cose@1" }
  key_providers: {
    "test-kp": {
      algorithms: ["Ed25519"]        // supported algorithms / key types (§17.6)
      operations: ["sign"]           // supported protected operations (§17.5)
      generate: true                 // supports automatic generation (§17.6)
      bind: false                    // supports external binding (§17.4, §17.6)
      protection: "software"         // declared protection class (§17.1, §17.6)
      external_keys: {               // externally created handles for manual bind
        "ext1": { algorithm: "Ed25519" }
      }
    }
  }
}
```

- `hosts.namespaces` maps a registered namespace name to the contract it
  provides (§16.4). The simulated `cose` namespace implements `liasse.cose@1`:
  `cose.sign(ring, claims)` returns a bytes token signed by the ring's current
  active version (§17.7); `cose.verify(ring, bytes)` verifies against that
  ring's accepted public versions and yields a typed proof exposing the claims
  and the verified key-version identity (§17.7).
- `hosts.key_providers` maps a registered provider name (§17.5, §23.4) to the
  capabilities it advertises for the load-time checks of §17.6.

## Corpus readings (shape assumptions)

- Keyring public metadata is read through ordinary package views over
  `ring.$current`, `ring.$accepted`, `ring.$versions` (§17.2 exposes them as
  application-readable metadata).
- `$versions` and `$accepted` are matched as unordered arrays of
  version-metadata objects. §17.2 does not pin the container shape, nor the
  member names for "public key", "provider metadata", or attestation, so every
  version matcher carries `"...": true` and asserts only
  `id`, `algorithm`, `created_at`, `activated_at`, `retired_at`, `revoked_at`.
  This member-name gap is a spec ambiguity in its own right.
- Due rotations: §17.4 lets a runtime schedule rotation or perform a due
  rotation before the next operation, with identical logical order and key
  selection. Cases therefore perform an admission-creating operation after
  `advance_time` before asserting rotated state, and never exact-match
  rotation timestamps (`retired_at` etc. may reflect the scheduled instant or
  the triggering instant — see `red/retain-boundary-instant-unspecified`).
  Retention-window arithmetic in the cases is chosen to be correct under
  either reading.
- `connect` without `authenticate` opens an unauthenticated connection used
  for public surfaces (needed by the concurrency case).
- Role-surface addresses use `<role>.<surface>[.<mutation>]`, mirroring
  FORMAT.md's `public.<surface>.<mutation>`.
