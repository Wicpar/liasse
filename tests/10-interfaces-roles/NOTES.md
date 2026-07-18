# §10 External interfaces and roles — corpus notes

Chapter directory for SPEC.md §10 (`#interfaces`). This file documents the
addressing conventions and step-vocabulary extensions used by the cases in
`common/` and `red/`, per FORMAT.md ("A chapter may need an action this
vocabulary lacks").

## Addressing convention for `call` / `watch` targets

FORMAT.md shows only `call: "public.tasks.add"`. This chapter extends the
same dotted form symmetrically:

- `public.<surface>` / `public.<surface>.<call>` — public surfaces (§10.2).
- `<role>.<surface>` / `<role>.<surface>.<call>` — role surfaces (§10.3).
  §11.4 requires every authenticated request to name its role; the first
  segment is that role name.

Segments are the exact declared names; no folding, trimming, or Unicode
normalization is implied (declared names are ASCII-only per §2.5).

## `authenticate` payload shape

FORMAT.md leaves the `authenticate` payload to "whatever mechanism the
case's package defines (§11)". All cases here use:

```hjson
{ connect: "c1", authenticate: { role: "member", auth: "token", credential: "alice" } }
```

mirroring the §11.4 request members (`role`, `auth`, `credential`). The
packages declare deliberately trivial authenticators (`$verify:
"$credential"`, `$actor: "/accounts[$proof]"`) because this chapter tests
role gating and exposure, not credential verification (§11 territory).
An `authenticate` step may carry a step-level `expect` like any other action
step (used by `red/role-rejects-unaccepted-authenticator`).

## Extension step members

No new step *keys* were needed; the following new *members* on existing
steps are used:

| member | on | meaning |
|--------|----|---------|
| `scope` | `call`, `watch` | canonical key text of the containing row for a **scoped role** target (§10.3: "An external request addresses a scoped role by its containing row identity and role name"). Absent for root-level roles and public surfaces. |
| `args` | `watch` | typed values for the surface `$params` (§10.1, §12.1 `view` operation). Omitted members take their declared defaults. |
| `expect` | `watch`, `authenticate` | outcome assertion for operations expected to fail before a subscription/init exists (same outcome vocabulary as `call` `expect`). |
| `descendant` | `call` | **hypothetical** — used only by `common/recursive-descendant-mutation-addressing-unspecified` to record that the spec does not define how a mutation call addresses a recursively covered descendant (§10.5). Harnesses should treat the case as `unspecified`, not implement the member. |

## Outcome-mapping convention: unresolvable / ungranted names → `denied`

The spec mandates that a request naming anything not exposed to the caller
must fail without executing (§10.1 "$mut maps external call names", §10.4
"A surface grants named access", §12.1 step 1 resolution). SPEC-ISSUES #8 now
pins the taxonomy: every such failure — nonexistent surface, undeclared call
name, internal declaration name, role the actor is not a member of,
authenticator not accepted by the role — is the `denied` class (FORMAT.md:
"rejected by authentication, roles, or permissions"); there is no distinct
not-found outcome. §10.4 further requires the denial to be *indistinguishable*
across existence: for a fixed caller and authentication context, a name that
does not exist and a name that exists but is not granted MUST deny identically
(class and diagnostic code), so a non-member cannot enumerate a role's surface
catalog. This is asserted in `red/unresolvable-name` (the `denied` class through
the step vocabulary) and, at the wire-code level, in
`crates/liasse-connect/tests/authz.rs`.

`rejected` is reserved for admission failures of an otherwise well-addressed
request (receiver cardinality per §10.1 "Zero or several selected rows
reject the request", checks, etc.).

## Matching looseness in recursive-view cases

Recursive-coverage expectations (`common/recursive-coverage-nests-included-descendants`,
`red/except-prunes-entire-branch`) add `"...": true` on node objects so the
cases do not pin the representation of an *empty* child view on leaf nodes
(`[]` vs. absent), which §10.5 does not specify. Array-level membership —
which children appear at each level — is still matched exactly, since that
is the normative content of `$where`/`$except`.

## Unicode payloads

`red/confusable-surface-name-does-not-resolve` and
`red/non-ascii-surface-name-invalid` intentionally contain CYRILLIC SMALL
LETTER A (U+0430) inside the name `tаsks`. Editors that normalize or
"fix" Unicode will destroy these cases; the files are UTF-8 and must be
kept byte-exact.
