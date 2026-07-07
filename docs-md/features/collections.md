# Collections and structs

## Static structs

A plain object is a fixed nested value on its containing row. Optional structs use `$optional` on the struct declaration.

## Keyed collections

A collection with `$key` stores rows with local identity.

```hjson
projects: {
  $key: id
  id: uuid
  name: text
}
```

Composite keys are declared by naming several key fields. Selectors use the collection's key type.

## Backend lowering

The logical tree is stable. A backend may flatten nested collections into tables, but authors keep writing paths against the logical model.
