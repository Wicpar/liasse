# §11 Authentication and sessions — corpus notes

Cases in this chapter exercise SPEC.md [§11 Authentication and sessions](../../SPEC.md#authentication),
together with the rules it leans on: role admission (§10.3/§10.5), the exactly-one-row
rule for `$actor`/`$session` contexts (§6.3), bucket visibility for session expiry
(§14.1/§14.2), and the client pipeline (§12.1–§12.3).

## Simulated host namespace `test.token`

Authenticators need a verifier namespace (§11.3, §16.3). Packages here declare
`$requires: { token: "test.token@1" }`, and each case supplies the simulated component
under `hosts.token`. Its deterministic contract:

- claims value type: struct
  `{ auth: text, session: uuid?, account: uuid?, name: text?, device: text?,
     issued_at: timestamp?, expires_at: timestamp? }`.
  `issued_at`/`expires_at` mirror the token claims signed in the spec's §11.5
  login example. They are self-asserted token metadata only: no authenticator
  in this chapter reads them for validity, which is exactly what
  `red/session-bucket-authoritative-over-token-claim` relies on.
- `token.sign(claims) -> text` — effect class `generated`. Returns an opaque, unique
  token text embedding exactly the given claims.
- `token.verify(credential: text) -> claims` — effect class `verifier`. Returns the
  claims embedded by `sign` for a token minted by this application, or the claims
  mapped in the case's `hosts.token.tokens` object for a literal pre-issued
  credential. **Any other credential fails verification with a diagnostic**, so
  authentication fails without a proof. `verify` performs no state mutation.
- `hosts.token.tokens`: map from literal credential text to claims. It models
  credentials signed out-of-band, so cases can seed sessions via `$data` without
  running a login mutation. A credential absent from this map and not produced by
  `sign` is by definition forged/tampered.

## Step vocabulary extensions (beyond tests/FORMAT.md)

- `auth` — extension **field** on `call` and `watch` steps:
  `auth: { auth: "<authenticator name>", credential: <value> }`.
  This is the explicit per-request authenticator selection plus credential of
  §11.4/§11.8. It overrides the connection default set by `connect.authenticate`
  (which uses the same object shape). The targeted role is the first segment of the
  call/watch address: role surfaces are addressed `<role>.<surface>.<mutation>` /
  `<role>.<surface>`, mirroring FORMAT.md's `public.<surface>.<mutation>`.
  An `auth` object that omits the `auth` member while carrying a credential models a
  request that fails to name its authenticator (§11.4). A step with no `auth` field
  on a connection that never authenticated models a fully unauthenticated request.
- `expect_close: { watch: "<id>" }` — extension **step**: asserts that the named
  subscription has received `close` (§12.2) once all prior steps' commits are
  reflected on its connection.
- `operation_id` — extension **field** on `call`: the optional high-entropy operation
  identifier of §12.3, used by replay/deduplication cases.

## Outcome conventions

SPEC.md defines no finer error taxonomy than FORMAT.md's outcome classes. All
authentication-stage and role-admission failures — `$verify` diagnostics, a credential
that cannot satisfy the declared `$credential` type, `$session`/`$actor` resolving zero
or several rows, `$check` failure, an authenticator the targeted role does not accept, a
request that does not name its authenticator, unauthenticated access to a role surface,
and `$members` failure — are asserted as `denied`, FORMAT.md's class for "rejected by
authentication, roles, or permissions". Cases assert the class only, never diagnostic
text or codes.

## Spec ambiguities captured as `outcome: unspecified`

- `red/public-surface-authenticator-selection-unspecified`: §11.4 says public
  surfaces "carry no authenticator selection" and §10.2 says a public operation
  "has no `$actor` or `$session`", but neither pins the outcome when a client
  *does* attach an authenticator selection + credential to a public address.
  Ignore-the-selection (proceed actor-less) and reject-the-malformed-request are
  both defensible; the outcome is genuinely unspecified.

## Known coverage gaps (see also structured report)

- §11.9 module-scope rules (child-exposed surfaces, parent authenticator aliases,
  "installing a module alone creates no external endpoint", parent-wraps-child actor
  propagation across a module boundary): SPEC.md never defines the syntax for parent
  authenticator aliases nor the mechanism by which a child surface is exposed
  "directly to clients", and FORMAT.md has no module-installation step. The
  scope-independent core (internal calls preserve `$actor`/`$session`, §11.1/§8.11)
  is covered by `common/internal-call-preserves-actor`.
- §11.4 `kid` key-version selection inside one authenticator: keyring/provider
  behavior (§17) not observable through the simulated verifier without inventing
  stub key-rotation semantics.
- §11.3 "Their sequence affects diagnostic reporting only" (check ordering): no
  observable semantic difference exists to assert without pinning diagnostics.
- §11.3 credential call-lifetime confinement ("External request arguments, including
  `$credential`, exist for the call lifetime"): non-persistence is not observable
  through any surface without asserting the absence of data in diagnostics/audit
  records, whose shape the spec does not pin.
