# §13 Modules — chapter notes

This directory covers SPEC.md §13 (anchor `#modules`, plus
`#1310-stateful-services-across-module-boundaries`).

## Extension steps

FORMAT.md has no step vocabulary for module lifecycle. §13.3 and §9.2 define
these as host-level lifecycle operations (they run outside any client
connection, create no actor, and each admitted operation is one atomic
commit). The cases in this chapter use:

| step | semantics |
|---|---|
| `module_install` | `{ module_install: { at: "<display path of the module collection>", request: { $name, $module, $config?, $data?, $use? } }, expect: {...} }`. Performs the §13.3 install into one entry of the named module collection. `$module` names a package `name@version`; it resolves against the case's `packages` map by each entry's declared `$module` value. Explicit `$use` bindings name a sibling instance by NAME (§13.3/§13.5: peer lookup stays within the sibling set, which is that collection). **Canonical registry step owned by this chapter** (see the Extended step registry in `tests/FORMAT.md`); other chapters (e.g. §19 child-module cases) reference it. |
| `module_uninstall` | `{ module_uninstall: { instance: "<display path>" }, expect }` — §13.3/§13.12 uninstall through the ordinary cross-module deletion plan. |
| `module_disable` | `{ module_disable: { instance: "<path>" }, expect }` — §13.3/§13.12 disable. |
| `module_enable` | `{ module_enable: { instance: "<path>" }, expect }` — §13.3 enable (revalidate and restore). |
| `module_update` | `{ module_update: { instance: "<path>", to: "name@version" }, expect }` — §13.14/§13.15 single-instance update; `to` resolves against the `packages` map like `$module`. On success `expect.value` matches the §13.15 update report. |
| `module_rename` | `{ module_rename: { instance: "<path>", to: "<new instance name>" }, expect }` — §13.3: renaming an instance is a rekey under the ordinary key-mutation rule. |
| `module_lifecycle_call` | `{ module_lifecycle_call: { mutation: "<$mut name>", args: { ... }, receiver?: [<key component>, ...], package?: "<packages label>", as?: "<blob parameter>" }, expect }` — admits a **host/root-scope** transition that carries module instances through their lifecycle (§13.10, §13.16), so the root program is lent the privileged handle. This is how a case drives the §13.16 value surface — `.modules[@id] <- unpack(@package)`, `pack`, `update_module`, `rollback_module` — or a `module.<op>` builtin. `package` names a `packages` entry the step serializes into a `.liasse` blob, stores in the root's §18.3 blob storage, and binds to the `as` parameter (default `package`), so the mutation's `unpack(@package)` reads exactly those bytes. Arguments decode against the mutation's own declared parameter types. `receiver` names the row a ROW mutation runs on, in `$key` order — §13.16's bare `.modules[@id]` is written relative to a receiver, so a case exercising that spelling supplies one. **Not reachable through `call`**: §13.10 lends the lifecycle privilege by scope, so an external `$public` caller is lent nothing — `red/lifecycle-operator-needs-host-privilege` is that refusal, and reaching for `call` here would silently test the wrong thing. Outcomes: `ok` = committed; `rejected` = the transition refused (a mount whose containing row is not live, a duplicate instance name, an unreachable instance, or any admission refusal inside the program). |

Private `$deps` requirements (§13.6) also resolve against the case's
`packages` map by package line and compatible major.

## Step-parameter clarifications

- `watch` steps in this chapter may carry `args: { ... }` — the typed view
  parameters of the subscribed surface (§10.1 `$params`, §12.1 `view`).
  FORMAT.md's `watch` example shows no parameter member, so this is
  documented here.
- `expect: { outcome: ok, value: "$absent" }` on a `call` asserts that the
  mutation is response-free (§13.8: omission of `$return` declares a
  response-free mutation): the call succeeds and carries no response value.

## Outcome conventions at install/update time

`module_install` and `module_update` validate a package definition and admit
a composition change in one operation (§13.3 "Loading validates ... before
the instance becomes active"). Cases use:

- `invalid` when the supplied definition, configuration typing, contract
  satisfaction, or another static validation rule fails (FORMAT "statically
  rejected at build/load/validation time");
- `rejected` when validation passes but admission against current state
  fails (duplicate instance name, unresolved or ambiguous peer bindings,
  seed/overlay values failing checks).

The spec does not itself classify these failures beyond "fails validation /
is rejected"; the split above follows the FORMAT.md vocabulary definitions.

A compatibility-narrowing update (§13.14) is classified `invalid`, not
`rejected`: §13.14 states a narrowing release is rejected by "package
loading" and that "a failing recheck blocks the update before admission".
FORMAT.md maps a pre-admission load/validation refusal to `invalid`
(admission-time refusals are `rejected`). This is a purely definitional
comparison of the old and new exposed compatibility surfaces, independent of
application/composition state, so it falls under "contract satisfaction /
static validation" like the interface-contract cases. Applies to
`common/minor-update-narrowing-rejected` and
`red/update-narrowing-view-field-rejected` (filenames keep "rejected" in the
plain-English sense of "the release is refused").

## Authoring-form assumptions

- A ref whose target is an imported peer interface is written
  `{ "$ref": "#<handle>" }` (optionally with `$on_delete`). Annex A.2 admits
  `#surface.$key` as a key type and §13.12 normatively requires `$on_delete`
  on refs crossing module boundaries, but §13 shows no concrete `$ref`
  authoring example for it. Two cases rely on this form, both only to
  exercise the §13.12 cross-boundary `$on_delete` rule:
  `red/cross-boundary-ref-missing-on-delete-invalid` (omits `$on_delete`, so
  the package is non-conforming) and
  `red/uninstall-blocked-by-cross-boundary-restrict-ref` (declares
  `$on_delete: restrict` so uninstall of the referenced peer is blocked).
- A per-instance interface view bound to a literal instance name
  (`.modules["kit"]::templates`) that names an instance which does not exist
  — because it was never installed or was renamed away — selects nothing and
  yields an empty collection, under the ordinary absent-keyed-selection rule.
  `red/rename-instance-stale-name-not-addressable` relies on this to show the
  old name is no longer addressable after a rekey.
- Parent code addresses one instance's interface as
  `.modules["<name>"]::<interface>` and an interface mutation as
  `.modules["<name>"]::<interface>.<mutation>` (grounded in §13.9, §13.10
  `#credits.consume`, §13.11 `#billing.invoices.create`, and the W4 worked
  example `.modules[@module]::templates[@template]`).
- An `$expose` (or a module collection's `$expose`) `$mut` binding value MAY be an inline
  single-statement mutation expression, not only a named-`$mut` reference.
  §13.8 says a module "binds that contract to private views and mutations",
  and §3.2 / §8 show a bare insert such as `.tasks + { title: @title }` as a
  valid mutation statement; the spec's own bindings show both a named ref
  (`.create_template`) and a mutation-call expression
  (`.templates[@template].disable()`). `common/if-module-guarded-state-preserved`
  binds the response-free `add` contract to `.notes + { id: @id, note: @note }`
  inside the `$if_module`-guarded `$expose` block, so the binding activates and
  deactivates together with the guarded interface (a named model `$mut` cannot
  be guarded this way without dangling when the guard is off).
