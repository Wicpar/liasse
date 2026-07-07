# Checks and transforms

Status: locked for this draft.

This document covers checks and value transforms. Access, view, ref, and mutation syntax lives in `SYNTAX.md`.

## 1. Current value

`.` means the current scalar value in contexts with one obvious scalar value.

Examples:

```hjson
"email": {
  "$type": "text",
  "$normalize": "lower(trim(.))"
}
```

```hjson
"name": {
  "$type": "text",
  "$check": ["size(trim(.)) > 0", "Name is required"]
}
```

```hjson
"display_name": {
  "$type": "text",
  "$from": "name",
  "$as": "trim(.)"
}
```

Rules:

```text
in $normalize, . is the incoming value being normalized
in field-level $check, . is the value being checked
in $as migration transforms, . is the value read from $from
```

Named parameters use `@name`. Imports use `#name`. Bindings use bare names.

## 2. Check semantics

Checks use CEL assertion semantics.

Canonical form:

```hjson
"$check": "assert(size(trim(.)) > 0, 'Name is required')"
```

Tuple shorthand:

```hjson
"$check": ["size(trim(.)) > 0", "Name is required"]
```

Multiple checks:

```hjson
"$check": [
  ["size(trim(.)) > 0", "Name is required"],
  ["size(.) <= 320", "Value is too long"]
]
```

Tuple form expands to:

```text
assert(condition, message)
```

The condition evaluates to `bool`. The message evaluates to `text`.

## 3. Failed checks

A failed check rejects the value at validation time or rejects the proposed write before commit admission, depending on where the check runs in the normal write pipeline.

The diagnostic includes:

```text
path
check expression
message
source span when available
```

Example:

```json
{
  "path": "/companies/acme/users/fred/name",
  "check": "size(trim(.)) > 0",
  "message": "Name is required"
}
```

## 4. Normalization

`$normalize` is a write-time transform.

```hjson
"email": {
  "$type": "text",
  "$optional": true,
  "$normalize": "lower(trim(.))"
}
```

Every write to `email` stores the normalized value.

Rules:

```text
normalization is pure
normalization runs before checks on the normalized field
normalization reads only the current value and pure functions
```

## 5. Migration transforms

A target schema can describe how old data becomes new data.

```hjson
"display_name": {
  "$type": "text",
  "$from": "name",
  "$as": "trim(.)"
}
```

Meaning:

```text
during upgrade, read the old field name, bind it as ., transform it with trim(.), and write display_name
```

If `$as` is omitted, the value is copied as-is.

```hjson
"display_name": {
  "$type": "text",
  "$from": "name"
}
```

Collection rename uses the same key on the collection declaration:

```hjson
"clients": {
  "$from": "customers",
  "$key": "id",
  "id": "text",
  "name": "text"
}
```

Backward direction: mechanical transforms (`$from` without `$as`, renames,
added fields with defaults) invert automatically. A lossy `$as` may declare
its inverse:

```hjson
"display_name": {
  "$type": "text",
  "$from": "name",
  "$as": "trim(.)",
  "$back": "."
}
```

Downgrade rule: downgrading installs the older package. Values the older
shape can hold are restored — from `$back` where declared, from history for
rows unchanged since the upgrade — and everything unrepresentable is
archived, never dropped. History is never rewritten.

Rules:

```text
$from names the old field or old path relative to the current row/collection shape
$as is a CEL expression over .
$back is the optional inverse of $as
checked transforms are planned before data changes
failed row transforms are reported with row identity and path
```

## 6. Purity and generative functions

Expressions split into two evaluation moments:

```text
fold-time expressions
  $check, $normalize, computed fields, $view, $expose
  must be pure: same inputs, same result, on every engine, at every replay

write-time expressions
  $default, $data values, mutation expressions and their arguments
  may call generative functions: uuid(), now()
```

Generative results are produced once, when the commit is created, and the
produced values are recorded in the commit. Replay and history folds reuse
the recorded values, so the fold stays deterministic while authors keep
`"$default": "= uuid()"` and `"created_at": "= now()"`.

A generative function in a fold-time position is a load-time diagnostic.
