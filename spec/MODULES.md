# Module spec

Status: working draft. This document contains module-related concepts only. General access/view/mutation syntax lives in `SYNTAX.md`.

## 1. Concepts

Liasse has two dependency kinds, plus module-space interfaces.

```text
required dependency ($deps)
  a private nested instance of another package, owned by the consumer;
  coupled by the interfaces it fills, never shared with siblings

peer dependency ($use)
  a binding to a sibling instance in the same module space, installed
  independently by the operator; coupled structurally at usage sites

internal module interface
  a module space declares interfaces that installed modules may expose
```

Core keys:

```text
$deps
  private required dependencies, instantiated inside the consumer

$use
  binds peer dependencies (sibling instances) and parent-provided surfaces

$modules
  declares a module space at this exact location

$modules.$expose
  exposes parent surfaces to modules installed in that module space

$modules.$interfaces
  declares interfaces that installed modules may expose back to the module space

$expose
  maps an installed module's local model/data to one of the containing module space's interfaces
```

## 2. Module packages

A module is a package with private schema and data.

```json
{
  "$liasse": 1,
  "$module": "acme.fr_sales_templates@1.0.0",

  "$model": {},
  "$data": {},

  "$use": {},
  "$expose": {}
}
```

Rules:

```text
$module identifies the package and its version
$model is private schema owned by the package
$data is initial/imported data matching $model
$use declares imports the module needs
$expose declares data surfaces exported to the containing module space
```

A module may be schema-heavy, data-heavy, or both.

## 3. Module spaces

A field declared with `$modules` creates a module space.

```json
{
  "$model": {
    "companies": {
      "$key": "id",
      "id": "text",
      "name": "text",

      "modules": {
        "$modules": {}
      }
    }
  }
}
```

Location is scope.

This creates separate module spaces:

```text
/companies/acme/modules
/companies/globex/modules
```

A module installed in `/companies/acme/modules` is local to Acme's module space. Shared modules live at a shared location:

```json
{
  "$model": {
    "shared_modules": {
      "$modules": {}
    },

    "companies": {
      "$key": "id",
      "id": "text",
      "name": "text"
    }
  }
}
```

## 4. Parent-provided surfaces

A module space may expose surfaces from its containing row to installed modules.

Use `$modules.$expose`.

```hjson
"companies": {
  "$key": "id",
  "id": "text",
  "name": "text",
  "plan": "text",

  "$mut": {
    "rename({ name: text })": ".name = @name",
    "set_plan({ plan: text })": ".plan = @plan"
  },

  "modules": {
    "$modules": {
      "$expose": {
        "company": {
          "$view": ". { id, name, plan }",
          "$mut": ["rename", "set_plan"]
        }
      }
    }
  }
}
```

Meaning:

```text
modules installed in this module space may import a parent-provided surface named company
the surface contains the view fields id, name, and plan
the surface exposes the parent mutations rename and set_plan, which must be declared on the containing row shape
```

The surface is row-local. Under `/companies/acme/modules`, `company` is Acme. Under `/companies/globex/modules`, `company` is Globex.

## 5. Importing parent-provided surfaces

A child module imports parent-provided surfaces through normal `$use` entries.

```json
{
  "$use": {
    "company": "$parent"
  }
}
```

This imports the parent-provided surface named `company` as `#company`.

The shorthand:

```hjson
"company": "$parent"
```

expands to:

```hjson
"company": "$parent.company"
```

because the local handle name is `company`.

Renaming the handle:

```json
{
  "$use": {
    "org": "$parent.company"
  }
}
```

The module reads:

```text
#org.id
#org.name
#org.plan
```

Member selection and renaming uses view syntax when needed:

```json
{
  "$use": {
    "org": "$parent.company { org_id: .id, display_name: .name, plan }"
  }
}
```

The module reads:

```text
#org.org_id
#org.display_name
#org.plan
```

## 6. Peer dependencies

A peer dependency binds a handle to a **sibling instance** in the same
module space — installed independently, versioned freely.

```json
{
  "$use": {
    "people": "acme.people/people"
  }
}
```

This binds a sibling exposing the `people` surface as `#people`. An `@n`
suffix is an optional operational pin narrowing candidates; it is not the
contract.

**The contract is structural, at usage sites.** The consumer never
declares what it expects from a peer — it uses `#people` where needed
(`#people.totals { member, minutes }`), and the checker validates every
usage site against the bound sibling's live `$expose` surface at bind time and
on every update. The usage site is the declaration. A consumer that
tolerates different peer shapes writes it inline, detecting by what is
exposed:

```text
has(#vh.totals) ? #vh.totals { member, minutes }
                : #vh.ledger[:e | e.kind == 'total'] { member, minutes: e.mins }
```

Optional direct imports live under `$optional`.

```json
{
  "$use": {
    "people": "acme.people/people@1",

    "$optional": {
      "billing": "acme.billing/customers@1"
    }
  }
}
```

Access:

```text
#people
#billing
```

Rules:

```text
peer bindings are only valid toward siblings of the same module space
optional peers may be absent; absence propagates (below)
if a module needs two surfaces of the same kind, it names two handles
```

```json
{
  "$use": {
    "buyer": "acme.people/people@1",
    "seller": "acme.people/people@1"
  }
}
```

Resolution:

```text
candidates are instances exposing a compatible public surface, visible by
  walking up the ancestor module spaces from the install location
exactly one candidate  -> bound automatically
several candidates     -> install requires an explicit binding
zero candidates        -> required import blocks install; optional stays absent
the binding is persisted in the install record and rebindable by the operator
```

Optional import semantics — absence propagates:

```text
has(#billing) tests presence
any expression reading an absent import yields absent
a computed field, view, or exposed surface whose expression reads an absent
  optional import is itself absent
stored private data is unaffected: nothing is archived or migrated when an
  optional import appears or disappears; dependent derived surfaces simply
  reappear
```

Because only derived surfaces depend on imports, optional imports need no
lifecycle machinery at all.

## 6b. Required dependencies: `$deps`

A required dependency is a **private nested instance** owned by the
consumer — its own data, its own migrations, invisible to siblings.
Coupling is by the interfaces the dependency declares it fills:

```json
{
  "$deps": {
    "tax": "acme.tax@2"
  }
}
```

```text
private: nothing is shared — two siblings may hold different majors of
  the same package; conflict is unrepresentable
interface-coupled: the dependency's package exposes the interfaces it
  implements; the consumer couples to the interface, so a dep update that
  still exposes it is transparently compatible, and one that stops exposing
  it is structurally unresolvable — versions are operational pins, not the
  contract
version flow: minors and patches follow automatically (compatible by the
  computed surface); the pinned major changes only when the consumer's
  own release requires it, at which point the dep's own migration plan
  runs on the private instance as part of the consumer's update
attenuation: a dep's $use/$deps needs are granted from the consumer's
  own granted world — a dependency can never reach beyond what its
  consumer was given; the consumer's install prompt shows the transitive
  closure of needs, once
internal: a private dep's exposes and surfaces are the consumer's
  internal machinery, never operator-visible — to use a package as a
  product, install it as a sibling and peer with it
```

## 6c. `$if`: conditional declarations

Any declaration block may be guarded by a bare expression, typically over
presence:

```hjson
"$if": "has(#vh)"
```

```text
an inactive block's declarations do not exist: fields absent, views absent,
  exposes absent, mutations uncallable
data owned by declarations inside the inactive block is archived on
  deactivation and restored on reactivation
optional import absence is separate: it only makes dependent derived
  declarations absent and never archives the module's private data
$if generalizes and replaces the former $with construct; definitions stay
  where they are used, and variability is written inline, never as
  version-case declarations
```

## 7. Module-space interfaces

A module space declares interfaces that installed modules may expose.

```hjson
"modules": {
  "$modules": {
    "$interfaces": {
      "ecriture_templates": {
        "$key": "id",
        "id": "text",
        "label": "text",
        "journal": "text",
        "lines": "json"
      },

      "tax_rules": {
        "$key": "id",
        "id": "text",
        "country": "text",
        "rule": "json"
      }
    }
  }
}
```

An interface value is a type — named from `$types` or inline — used in
contract position:

```hjson
"$interfaces": {
  "ecriture_templates": "ecriture_template_bank",
  "tax_rules": { "$key": "id", "id": "text", "country": "text", "rule": "json" }
}
```

Rules:

```text
interface names are local to the declaring module space
an interface is a type in contract position; satisfaction is structural:
  an exposed view satisfies it iff its rows satisfy the shape and its
  identity maps onto the declared $key
installed modules expose declared interfaces; shape checked at
  install/update
interface shape changes are versioned with the package that declares the
  module space
the same rule covers every contract position: module-space interfaces,
  meter pool interfaces, blob parameter types, and client $params
```

## 8. Exposing module data to a module-space interface

An installed module maps private data to a containing module-space interface with `$expose`.

Simple form:

```json
{
  "$expose": {
    "ecriture_templates": ".templates"
  }
}
```

Filtered/projection form:

```json
{
  "$expose": {
    "ecriture_templates": ".templates[:template | template.enabled] { id, label, journal, lines }"
  }
}
```

Field adaptation uses normal view projection:

```json
{
  "$expose": {
    "ecriture_templates": ".templates[:template | template.enabled] { id, label: template.display_name, journal, lines }"
  }
}
```

Rules:

```text
$expose values are view expressions over the module's private model/data
the exposed result satisfies the containing module space's interface declaration
private data is visible only through declared exposed interfaces
if an exposed surface lists mutations, those mutations must already pass
  static delete-completeness in the exporting package; crossing the module
  boundary never supplies an implicit $on_delete policy
```

## 9. Views into module spaces

A parent row can create views over interfaces exposed by modules installed in its module space.

All modules exposing an interface:

```hjson
"available_ecriture_templates": {
  "$view": ".modules::ecriture_templates"
}
```

Projected aggregate:

```hjson
"available_ecriture_templates": {
  "$view": ".modules::ecriture_templates { id, label, module: modules.$key, template: ecriture_templates.$key }"
}
```

One installed module:

```hjson
"fr_sales_templates": {
  "$view": ".modules[\"fr_sales\"].ecriture_templates"
}
```

Selected installed modules:

```hjson
"selected_templates": {
  "$view": ".modules[.enabled_template_packs]::ecriture_templates"
}
```

Identity for aggregate module views comes from the source row chain:

```text
modules.$key + ecriture_templates.$key
```

## 10. Module updates

Each installed module instance records its own package version and migrates independently.

```text
/companies/acme/modules/fr_sales      acme.fr_sales_templates@1.0.0
/companies/globex/modules/fr_sales    acme.fr_sales_templates@1.1.0
```

**The update rule, set in stone.** Any instance update — its own
version, a `$deps` major, or a peer target's update — triggers a full
structural recheck: every inbound peer binding's usage sites against the
new exposed surfaces, every `$deps` coupling against its interfaces.

```text
all sites resolve  -> the update proceeds as one atomic per-instance
                      commit; $deps instances run their own migration
                      plans inside it; minors/patches of deps and peers
                      can never fail this (the compatibility surface
                      cannot break within a version)
any site fails     -> the update is BLOCKED before commit admission, with
                      a report naming the binding, the usage sites, and the
                      shape mismatch — never auto-detach, never silent degradation
```

The operator's remedies are all explicit:

```text
update the consumer first
unbind an optional peer — consented degradation via absence and $if
install the new major side by side as a new sibling instance (legal by
  instance independence) and migrate consumers gradually
```

Author obligations on release: ship transforms for your own shape
changes; keep filling the interfaces you claim, or accept that dropping
one is the breaking act that blocks where used. Consumers owe nothing —
they declared nothing.

Other rules:

```text
updating one instance updates that instance only
views over module spaces continue to use the row-local module space
```

## 11. Install record

The persisted install record should include at least:

```json
{
  "$module": "acme.fr_sales_templates@1.0.0",
  "$source": "sha256:...",
  "$resolved": {
    "company": "$parent.company"
  }
}
```

Locked shape:

```json
{
  "$module": "acme.fr_sales_templates@1.0.0",
  "$source": "sha256:<hash of the canonical package document>",
  "$resolved": {
    "company": "$parent.company",
    "people": "/shared_modules/people#people"
  },
  "$absent": ["billing"],
  "$migrations": [
    { "$from": "0.9.0", "$to": "1.0.0", "$commit": "<commit id>" }
  ]
}
```

Rules:

```text
$module    package id and exact installed version
$source    hash of the canonical package document, verifying provenance
$resolved  one entry per bound import: $parent.<surface> or
           <instance path>#<surface>
$absent    optional imports currently unbound
$migrations applied package migrations with their commits
```

## 12. Module instance data and seeds

An installed instance is its install record plus its private data, nested at
the instance path — inside every export of the containing row:

```hjson
"modules": {
  "fr_sales": {
    "$module": "acme.fr_sales_templates@1.0.0",
    "$source": "sha256:…",
    "$resolved": { "company": "$parent.company" },
    "templates": {
      "sale_invoice": { "label": "Facture de vente", "journal": "VE" }
    }
  }
}
```

`$data` seed semantics:

```text
on install, $data rows are applied as ordinary inserts; = expressions are
  write-time expressions evaluated with the instance's imports bound
on update, changed seed rows are re-applied by three-way merge
  (old seed, new seed, current row): local modifications win
seed application is part of the install/update commit
```

## 13. Update report

A successful `modules.update` returns:

```json
{
  "$instance": "/companies/acme/modules/fr_sales",
  "$from": "1.0.0",
  "$to": "1.1.0",
  "$migrated": [ { "$path": "templates", "$rows": 12 } ],
  "$seeded": [ { "$path": "templates", "$added": 2, "$merged": 1, "$kept_local": 1 } ],
  "$exposed": { "$unchanged": ["ecriture_templates"], "$changed": [], "$removed": [] },
  "$imports": { "$rebound": [], "$broken": [] },
  "$archived": [],
  "$commit": "<commit id>"
}
```

Updating one instance updates that instance only; exposed interfaces are
rechecked against the module space's declarations before the update commit is
admitted. A breaking recheck rejects the update before admission and returns a
failure report with the same planning fields plus `$rejected`, but no `$commit`.
