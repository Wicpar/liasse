# Part VI Worked examples (W1–W4) — chapter notes

The worked examples in SPEC.md Part VI (§ "W1"–"W4", lines ~3881–4291) are
**informative** (§2.3). Each one is a whole-application illustration that
composes normative rules from other chapters. This corpus therefore does not
test "the worked example" as such; it turns each worked example into an
**end-to-end scenario** whose every expectation is deducible from the
*normative* chapters the example instantiates. Every case cites both the
worked-example anchor (`W1`…`W4`) and the normative anchor(s) that actually
pin the expectation.

Isolated single-rule behavior (one bucket boundary, one ref resolution, one
meter order) is already covered by the topic chapters (`05`, `08`, `10`,
`11`, `13`, `14`, `15`). Cases here deliberately exercise the **interaction**
of those rules along the path a real client walks: add→normalize→view,
login→authenticate→revoke, subscribe→spend→expire, install→expose→aggregate.

## Simulated host components (`hosts`)

FORMAT.md leaves the `hosts` shape to each chapter. W2 requires the external
`webauthn`, `cose`, and a key provider. To keep every case self-contained and
its expectations deducible without inventing keyring/COSE cryptographic
behavior, this chapter substitutes two deterministic simulated namespaces —
exactly the substitution the §11 corpus makes — and documents it here.

### `test.token` (stands in for `cose` + the `session-hsm` keyring)

Reused verbatim from the §11 corpus. Contract:

- claims type: struct
  `{ auth: text, session: uuid?, account: uuid?, name: text?, device: text? }`.
- `token.sign(claims) -> text` (effect class `generated`): returns an opaque,
  unique token embedding exactly the given claims. Used where W2 writes
  `cose.sign(/session_keys, {...})`.
- `token.verify(credential: text) -> claims` (effect class `verifier`):
  returns the claims of a token minted by this application (via `sign`), or the
  claims mapped in `hosts.token.tokens` for a literal pre-issued credential.
  Any other credential fails verification (no proof → authentication fails).
  Performs no state mutation. Used where W2 writes
  `cose.verify(/session_keys, $credential)`.
- `hosts.token.tokens`: map from literal credential text to claims, for
  seeding sessions without running a login.

Substituting `token.*` for `cose.*` + keyring preserves every rule the W2
scenario is about: session opening (§11.5), the `$verify`/`$proof`/`$session`/
`$actor` binding chain (§11.3), role admission (§10.3, §11.4), and revocation
/ expiry (§11.7). It drops only the keyring's key-version machinery, which the
§17 corpus owns.

### `test.webauthn` (stands in for `webauthn`)

- `webauthn.verify(response: text) -> { rp: text, credential: text }`
  (effect class `verifier`): resolves `response` to the identity mapped in
  `hosts.webauthn.responses`; an unmapped response fails verification. Models
  W2's `identity = webauthn.verify(@response)` yielding `identity.rp` /
  `identity.credential`.

## Step-vocabulary reuse (documented in the topic chapters)

This chapter introduces **no new step keys**. It reuses extensions already
defined and documented by the topic chapters, repeated here for locality:

- `auth` field on `call`/`watch` (from §11 NOTES): explicit per-request
  authenticator selection + credential,
  `auth: { auth: "<name>", credential: <value> }`; overrides the connection
  default from `connect.authenticate`. Role surfaces are addressed
  `<role>.<surface>[.<mutation>]`, mirroring `public.<surface>.<mutation>`.
- `operation_id` field on `call` (from §11 NOTES): the §12.3 high-entropy
  operation identifier, for replay / deduplication cases.
- `watch … args: { … }` (from §13/§14 NOTES): typed `$params` values for the
  subscribed surface (§10.1, §12.1).
- `module_install` / `module_uninstall` / `module_disable` / `module_enable`
  (from §13 NOTES): host-level module lifecycle. `module_install` carries
  `space` (the display path of the target module space) and a `request`
  object holding the §13.3 install request (`$name`, `$module`, optional
  `$config`/`$data`/`$use`). `module_uninstall` / `module_disable` /
  `module_enable` carry `instance`: the display path of an existing installed
  instance (`<space>/<name>`). `$module` resolves against the case's
  `packages` map by declared `$module` value. Each admitted lifecycle
  operation is one atomic commit that creates no actor (§11.1, §13.3).

## Outcome conventions

Following FORMAT.md and the topic-chapter conventions:

- `denied` — any authentication-stage or role-admission failure (§11).
- `rejected` — admission-time failure: a mutation receiver selecting zero or
  several rows (§10.1), a ref/lookup resolving zero rows (§6.3), a check or
  assertion failing (§8.8), insufficient metered capacity (§15.2), a
  non-advancing/negative interval (§14.5), a duplicate composite key (§5.4),
  or a module install whose overlay/binding fails admission against current
  state (§13.3; see §13 NOTES for the invalid-vs-rejected split).
- `invalid` — static (build/load) rejection: an undeclared exposed member or
  a reference to a private module path (§13.8).
- Read-only mutations ending in `return` that write no state complete with the
  `unchanged` status carrying the evaluated response; the corpus asserts these
  as `outcome: ok` with the returned `value` when a response is produced, and
  notes when the point under test is specifically the `unchanged`/no-commit
  status (§8.9, §12.3).

## Wire-value conventions (Annex A)

- `decimal`/`int` are canonical digit strings (`"30"`, `"150"`).
- `timestamp` generated by `now()`/`now()+duration` is matched with
  `"$any:timestamp"` rather than pinned, to avoid encoding microsecond
  arithmetic; seeded timestamps in `$data` use ISO-8601 strings (the §11
  authoring form).
- `uuid` generated by `uuid()` is matched with `"$any:uuid"`; a value reused
  across steps is captured with `"$bind:NAME"` and replayed with `"$ref:NAME"`.
- `none` (unbounded `$until`, absent optional) is **absence**: the member is
  omitted from an expected object under FORMAT exact-object matching.

## Spec ambiguities captured (outcome: unspecified)

- **`funding` projected metadata shape** (`w3/funding-projected-metadata-shape-unspecified`):
  §15.3 says every funding row records "any projected funding metadata used by
  the response," and W3's source projects `price`, but the normative example
  funding view (§15.3) lists only `{ source, pool, amount }`. Whether `price`
  is a member of each returned funding row is not pinned.
- **Unicode-whitespace trimming** (`w1/title-unicode-whitespace-trim-unspecified`):
  W1's `$normalize: "string.trim(.)"` — Annex A / §6 do not pin whether
  `string.trim` strips non-ASCII Unicode whitespace (e.g. U+00A0), so whether a
  title of only U+00A0 normalizes to empty (and fails the size check) is
  unspecified.
- **Seed-time computed field re-evaluation**
  (`w4/seed-computed-enabled-reevaluation-unspecified`): W4's module seed sets
  the stored field `enabled: "= #company.plan == 'fr-pcg'"`. §13.13 applies
  seed as inserts resolving computed values at insertion, but does not state
  whether a stored field seeded from a `#company` expression is re-evaluated
  when the parent later changes `plan`.
