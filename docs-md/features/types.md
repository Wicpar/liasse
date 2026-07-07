# Types

Types describe both stored values and expression values.

## Primitive types

`text`, `bool`, `int`, `decimal`, `uuid`, `date`, `timestamp`, `duration`, `json`, `blob`, and enum labels have canonical wire forms. Integers and decimals are encoded canonically as strings to keep hashing and database behavior stable.

## Containers

- `$set` stores unique unordered values.
- `set<T>` describes expression-level unique membership.
- `list<T>` is available for parameters and expression results.
- `view<T>` is a row stream with identity.
- `optional<T>` appears when a value can be `none`.

## Shapes as types

Named shapes, collection keys, refs, and structural objects are all part of the type system. This is what lets module interfaces and client surfaces be checked structurally.
