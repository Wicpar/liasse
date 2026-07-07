# Mutations

Status: working draft. This document gives mutation examples using the syntax defined in `SYNTAX.md`.

## 1. Operator meanings

```text
collection + view_expression
  insert rows into the collection

collection = view_expression
  replace the full collection

row_source { patch_block }
  patch every selected row

collection - keys
  delete rows by key values

-row_source
  delete selected rows

field = value_expression
  set a field

field -
  remove an optional field

set_field + value_expression
  add set member or members

set_field - value_expression
  remove set member or members

mutation({ args })
  call a mutation with named arguments
```

## 2. Patch blocks

Patch blocks use assignment syntax.

```text
{
  field = expr
  @field
  source.field
  .field
  -field
  nested { field = expr }
}
```

Shorthand examples:

```text
@name
```

expands to:

```text
name = @name
```

```text
import.name
```

expands to:

```text
name = import.name
```

```text
.name
```

expands to:

```text
name = .name
```

## 3. Patch selected rows

One row:

```text
.people[@id] {
  @name,
  -email
}
```

Many rows by typed keys:

```text
.people[@a, @b, @extra_people] {
  active = false
}
```

Filtered rows:

```text
.projects[:project | project.status == "draft"] {
  status = "archived"
}
```

Patch all rows in a collection:

```text
.people {
  active = true
}
```

Remove an optional field from every row:

```text
.people {
  -email
}
```

## 4. Insert rows

Insert one row from parameters:

```text
.people + {
  @id,
  @name,
  @email
}
```

Generated-key insert:

```text
.people + {
  @name,
  @email
}
```

Insert rows from a collection:

```text
.people + .imports {
  id,
  name,
  email
}
```

Insert from a module aggregate:

```text
.templates + .modules::ecriture_templates {
  module: modules.$key,
  template: ecriture_templates.$key,
  label,
  journal,
  lines
}
```

## 5. Replace a collection

```text
.people = .imports {
  id,
  name,
  email
}
```

```text
.templates = .modules::ecriture_templates {
  module: modules.$key,
  template: ecriture_templates.$key,
  label,
  journal,
  lines
}
```

```text
.people = []
```

The replacement result is normalized, checked, and keyed according to the target collection.

## 6. Delete by key

```text
.people - @id
```

```text
.people - @people
```

```text
.tax_rates - { country: @country, code: @code }
```

```text
.tax_rates - @rates
```

Keys from source data:

```text
.people - .imports { id }
```

Composite keys from source data:

```text
.tax_rates - .expired_rates { country, code }
```

## 7. Delete selected rows

```text
-.people[@id]
```

```text
-.people[@a, @b]
```

```text
-.people[:person | person.disabled]
```

```text
-.projects[:project | project.archived].tasks[:task | task.status == "open"]
```

## 8. Field and set mutation

Set field:

```text
.people[@id].name = @name
```

Replace object field:

```text
.people[@id].profile = {
  display_name: @name,
  locale: @locale
}
```

Patch object field:

```text
.people[@id].profile {
  display_name = @name,
  locale = @locale
}
```

Remove optional field:

```text
.people[@id].email -
```

Add/remove set members:

```text
.people[@id].tags + @tag
.people[@id].tags - @tag
.people[@id].reviewers + @person
.people[@id].reviewers - @person
```

## 9. Mutation calls

Calls use object arguments.

```text
#people.rename({ id: .lead, name: @name })
```

Shorthand:

```text
#people.rename({ @id, @name })
```

expands to:

```text
#people.rename({ id: @id, name: @name })
```

Arguments are matched by name.

## 10. Mutations declared on views

Views may declare mutations.

```hjson
"people_index": {
  "$view": ".people { id, name, email }",

  "$mut": {
    "rename({ id: uuid, name: text })": ".people[@id] { @name }",
    "import({ source: json })": ".people + @source.people { id, name, email }",
    "replace_all({ source: json })": ".people = @source.people { id, name, email }"
  }
}
```

Module-exposed view with mutation:

```hjson
"$expose": {
  "people": {
    "$view": ".people { id, name, email }",

    "$mut": {
      "rename({ id: uuid, name: text })": ".people[@id] { @name }"
    }
  }
}
```

## 11. Mutation sequences

A `$mut` body may be an array of mutation expressions. The array is one
atomic outcome: statements apply in order, later statements see earlier
effects, and any failure rejects the whole call before commit admission.

```hjson
"$mut": {
  "post({ journal: text, lines: json })": [
    ".entries + { journal: @journal, lines: @lines }",
    ".counters[@journal].last = .counters[@journal].last + 1",
    "assert(sum(.entries::lines[:l | l.side == 'debit'].amount) == sum(.entries::lines[:l | l.side == 'credit'].amount), 'Entry must balance')"
  ]
}
```

## 12. Concurrency: the read basis is what you read

There is no choice of write operator for concurrency. A proposed commit's
read basis is derived from evaluation:

```text
resolved selectors        the row existence and key values used
filters                   every row the condition examined
sources                   every source row a view expression consumed
assert()                  every value the condition read
```

At the proposal's canonical place in history order, the basis is re-verified.
If any read value changed, the proposal is rejected before admission. It does
not enter the DAG or history; the issuer rebases and retries.

Consequences:

```text
.people[@id].name = @name
  reads only row existence: concurrent name writes do not reject each other;
  the canonical order decides, last writer wins — the race with no true
  winner needs no ceremony

.projects[:p | p.status == "draft"] { status = "archived" }
  reads each examined row's status: a concurrent status change on a
  selected row rejects the proposal — the writer is never rugpulled

assert(.balance >= @amount, "insufficient funds")
  adds .balance to the basis deliberately: the withdrawal stands or falls
  with the balance it saw
```

Zero selected rows is not a conflict: the filtered mutation succeeds at
expression level. If the whole call lowers to an empty op tree, the engine
returns `unchanged` and admits no commit (§ SYNTAX 23). A keyed patch on a
missing row rejects, because a keyed patch targets an existing row.

## 13. `$on_delete` and static completeness

Deletion policy is declared on refs (HISTORY 7): `"restrict"` (explicit
rule, rejects while referenced), `"cascade"`, or an `= expr` patching the
containing row. There is no default: a mutation group, migration, or surface
that can delete a target whose inbound refs are undecided makes the model
fail to load, with the checker naming each ref. This includes cross-module
surfaces: a module may not expose a deleting mutation until all affected refs
have explicit policies. Cascades are live Postgres-style: a plain delete
traverses them; the lowered result is one atomic op tree — any failure rejects
everything before admission.
