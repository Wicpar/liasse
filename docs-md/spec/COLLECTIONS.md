# Collections and static structures

Status: working draft.

This document defines model shape: structs, keyed collections, sets, relations, uniqueness, and sort. Access, view, ref, and mutation syntax lives in `SYNTAX.md`.

Backend rule:

```text
The logical model is a tree.
The backend may flatten keyed collections, sets, and nested structs into relational storage.
Flattening is invisible to authors.
```

## 1. Package shape

A Liasse package has a model and data.

```json
{
  "$liasse": 1,
  "$app": "acme.app@1.0.0",
  "$model": {},
  "$data": {}
}
```

For modules:

```json
{
  "$liasse": 1,
  "$module": "acme.pack@1.0.0",
  "$model": {},
  "$data": {}
}
```

Rules:

```text
$model declares schema
$data contains initial/imported data matching $model
```

## 2. Shape kinds

Inside `$model`, a field can be one of these shapes.

```text
"name": "text"                       primitive field
"name": { "$type": "text" }         expanded field
"owner": { "$ref": "#people", "$on_delete": "restrict" } reference field
"address": { ... }                   static struct
"users": { "$key": "id", ... }      keyed collection
"tags": { "$set": "text" }          set of unique scalar values
"modules": { "$modules": {} }       module space
"open_items": { "$view": "..." }    computed/read view
"kind": { "$enum": ["a", "b"] }      enum field
"subs": { "$like": "^" }             shape alias (recursion)
"total": "= .ht + .vat"              computed scalar field (type deduced)
```

A plain object is a static struct unless it contains a collection/view/module/set marker such as `$key`, `$set`, `$modules`, or `$view`.

## 3. Static structs

A static struct is a fixed nested shape within its containing row.

```json
{
  "$model": {
    "companies": {
      "$key": "id",
      "id": "text",
      "name": "text",

      "billing_address": {
        "line1": "text",
        "line2": { "$type": "text", "$optional": true },
        "city": "text",
        "country": "text"
      }
    }
  }
}
```

Rules:

```text
struct fields are part of the containing row
structs can contain fields, structs, sets, views, and nested collections
optional structs use $optional on the struct object
```

## 3b. Computed scalar fields

A field declared as an `=` expression is computed: read-only, derived from
the containing row, never stored, kept current by the engine.

```hjson
"tasks": {
  "$key": "id",
  "id": "text",
  "estimate": { "$type": "int", "$optional": true },
  "spent": { "$type": "int", "$default": "= 0" },
  "over": "= has(.estimate) && .spent > .estimate"
}
```

The type is deduced by the checker; an ill-typed expression rejects the
model. Computed fields participate in views, sorts, and checks like stored
fields, and are absent when their expression yields absent.

## 4. Keyed collections

A keyed collection is a set of rows with local identity.

```json
{
  "$model": {
    "companies": {
      "$key": "id",
      "id": "text",
      "name": "text"
    }
  }
}
```

Rules:

```text
$key names the row identity field or fields
keys are unique within the containing collection and containing parent row
nested collections use the path for ancestry
backend storage may flatten nested collections into physical tables with hidden parent identity
```

Nested collection:

```json
{
  "$model": {
    "companies": {
      "$key": "id",
      "id": "text",
      "name": "text",

      "offices": {
        "$key": "id",
        "id": "text",
        "name": "text",

        "rooms": {
          "$key": "id",
          "id": "text",
          "label": "text",
          "capacity": "int"
        }
      }
    }
  }
}
```

Logical paths:

```text
/companies/acme
/companies/acme/offices/paris
/companies/acme/offices/paris/rooms/main
```

## 5. Data for keyed collections

In `$data`, a keyed collection is authored as a map from key to row.

```json
{
  "$data": {
    "companies": {
      "acme": {
        "name": "Acme SAS"
      }
    }
  }
}
```

The map key supplies the key field. A row may repeat its key field for readability, and if it does, it must match the map key.

Composite-key data uses canonical encoded keys in JSON object member names. Expressions use typed key objects; string encoding belongs to package data and export paths.

Canonical key encoding: each key field, in `$key` order, is rendered in its
canonical scalar text form (text as-is, int/decimal canonical digits, bool
`true`/`false`, uuid lowercase hyphenated, date/timestamp ISO-8601); inside
text parts the characters `%` and `:` are escaped as `%25` and `%3A`; parts
are joined with `:`. Deterministic, reversible given the key type, stable
across engines, and used identically in path display.

## 6. Generated keys

A collection key may have a default/generator.

```json
{
  "$model": {
    "people": {
      "$key": "id",
      "id": { "$type": "uuid", "$default": "= uuid()" },
      "name": "text"
    }
  }
}
```

Generated keys support inserts that omit key fields:

```text
.people + { @name }
```

## 7. Composite keys

A collection may use more than one key field.

```json
{
  "$model": {
    "vat_rates": {
      "$key": ["country", "code"],
      "country": "text",
      "code": "text",
      "rate": "decimal"
    }
  }
}
```

Use composite keys when row identity is naturally a tuple at that level.

Expression selector:

```text
.vat_rates[{ country: @country, code: @code }]
```

The key type is available as:

```text
.vat_rates.$key
```

## 8. Sets of unique values

A set is a unique collection of scalar values or references used for payload-free membership.

```json
{
  "$model": {
    "companies": {
      "$key": "id",
      "id": "text",
      "name": "text",
      "tags": { "$set": "text" }
    }
  }
}
```

Data:

```json
{
  "$data": {
    "companies": {
      "acme": {
        "name": "Acme SAS",
        "tags": ["customer", "fr", "priority"]
      }
    }
  }
}
```

Rules:

```text
set values are unique within the containing row
array order in $data is authoring order
read order and canonical export order follow canonical value order
```

## 9. Relations

Pure relation/membership:

```hjson
"reviewers": {
  "$set": {
    "$ref": "#people",
    "$on_delete": "restrict"
  }
}
```

Relation with payload:

```hjson
"assignments": {
  "$key": ["project", "person"],

  "project": { "$ref": ".projects", "$on_delete": "restrict" },
  "person": { "$ref": "#people", "$on_delete": "restrict" },
  "role": "text"
}
```

Rules:

```text
a set of refs is for pure membership
a keyed collection is for relations with payload
relation uniqueness is provided by $key and optional $unique constraints
```

## 10. Additional uniqueness

`$key` defines row identity. `$unique` defines additional uniqueness constraints.

Collection-level form:

```json
{
  "$model": {
    "users": {
      "$key": "id",
      "$unique": ["email", ["country", "tax_id"]],
      "id": "uuid",
      "email": "text",
      "country": "text",
      "tax_id": "text"
    }
  }
}
```

Field shorthand:

```hjson
"email": {
  "$type": "text",
  "$unique": true
}
```

Rules:

```text
unique constraints are scoped to their collection
nested collection uniqueness is also scoped by parent row path
optional absent values do not participate in uniqueness
composite unique constraints apply when all fields are present
```

## 11. Sort

`$sort` declares deterministic read order for collections and views.

Default sort:

```text
$key ascending
```

Collection declaration:

```json
{
  "$model": {
    "people": {
      "$key": "id",
      "$sort": ["name", "id"],
      "id": "uuid",
      "name": "text",
      "created_at": "timestamp"
    }
  }
}
```

Descending sort:

```hjson
"$sort": ["-created_at", "name"]
```

Structured sort form:

```hjson
"$sort": [
  { "$by": "name", "$dir": "asc" },
  { "$by": "created_at", "$dir": "desc" }
]
```

View sort inside projection:

```hjson
"templates": {
  "$view": ".modules::ecriture_templates { id, label, module: modules.$key, $sort: [module, label] }"
}
```

Rules:

```text
$sort defines deterministic read order
$sort preserves identity
default order is by key
sort fields are fields of the row/view output or computable from visible bindings
nested collection sort is local to each parent row
composite key default order is lexicographic by canonical key-field order
```

## 12. Views as collection shapes

A view is data. Its row identity is inferred from its source row chain unless it declares a synthetic `$key`.

Pass-through view:

```hjson
"active_people": {
  "$view": ".people[:person | person.active]"
}
```

Projected view:

```hjson
"people_index": {
  "$view": ".people { id, name }"
}
```

Synthetic keyed view:

```hjson
"account_totals": {
  "$view": ".entries::lines { $key: account, account, debit: sum(group.debit), credit: sum(group.credit), $sort: [account] }"
}
```

## 13. Types are shapes

The primitive and container syntax is canonicalized in `SYNTAX.md` §1a. A
`$types` entry uses the exact declaration grammar of `$model` — scalars,
enums, structs, keyed collections (with `$key`, `$sort`, checks), sets, blob
types with acceptance parameters. There is no separate model type sublanguage.

```hjson
"$types": {
  "role":       { "$enum": ["admin", "member"] },
  "store_bank": { "$key": "id",
                  "id": "text", "connector": "text",
                  "params": "json", "enabled": "bool" },
  "company":    { "$key": "id",
                  "id": "text", "name": "text",
                  "members": { "$key": "user",
                               "user": { "$ref": "/users", "$on_delete": "restrict" }, "role": "role" },
                  "subcompanies": "company" }
}
```

**Using a type name is `$like` to its declaration.** A string field whose
value names a type takes that shape: a collection type makes a keyed
collection, a struct type a nested struct, a scalar type a scalar field.
Recursion is plain self-reference (`"subcompanies": "company"`), mutual
recursion included. Declare the shape once; use it at the root and inside
every company without inventing donor structures:

```hjson
"stores": "store_bank",
"companies": { "…": "…", "storage": { "stores": "store_bank" } }
```

**Typing is structural.** A name exists for reuse and readability, never
identity: anything of the right shape satisfies a contract, named or not.
A view satisfies a collection-shaped contract iff its rows satisfy the
shape and its identity maps onto the declared `$key`. Enum comparisons
remain load-checked (`m.role == 'adnin'` is a diagnostic).

Contracts throughout the language are types used in contract position —
module `$interfaces`, meter pool interfaces, blob parameter types, and client
`$params`. One grammar, two spellings, one satisfaction rule.

## 14. `$like`: the positional spelling

`$like` remains for anonymous recursion, pointing at an enclosing
declaration by position:

```hjson
"subcompanies": { "$like": "^" }
```

Prefer a named type whenever the shape is used twice or referenced from
elsewhere; linters flag absolute-path `$like` ("name it") — legal, but a
named type says the same thing without coupling to a location.
