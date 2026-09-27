# Liasse Builtins

The builtins of `liasse 0.0.1`, predeclared in every module.
[LANGUAGE.md](LANGUAGE.md) defines the syntax, and [SPEC.md](SPEC.md) the
runtime semantics.

Each builtin is given by its signature. `T` stands for any type, and `…` after
a parameter takes one or more arguments. `fn op for A(b: B): R` is the operator
`op` with a left operand of type `A` and a right operand of type `B`, and
`fn op for A(): R` the prefix operator `op` on `A`.

---

## 1. Types

| Type | Values | Text literal |
| --- | --- | --- |
| `Bool` | `true`, `false` | |
| `Int` | arbitrary-precision integers | |
| `Decimal` | exact decimals (SPEC Annex A.3) | |
| `Text` | sequences of Unicode scalar values | |
| `Bytes` | small binary values | two hexadecimal digits per byte: `"0aff"` |
| `Uuid` | 128-bit UUIDs | the 8-4-4-4-12 hexadecimal form of RFC 9562 |
| `Date` | Gregorian calendar dates | RFC 3339 full-date: `"2026-09-25"` |
| `Timestamp` | instants, in microseconds since `1970-01-01T00:00:00Z` | RFC 3339 date-time with `Z` or an offset |
| `Duration` | elapsed times, in microseconds | ISO 8601 with day and time components: `"P1DT2H"` |
| `Object` | dynamic values over the CBOR data model | |
| `Blob(storage: Text = "default")` | descriptors of binary content held out of band | |

SPEC Annex A.4 gives each type's CBOR encoding, and Annex B its order.

- `Object`: a literal, record, or array in an `Object` position builds one;
  `x.name` is an `Object?`; the namespace `object` reads typed values.
- `Blob`: the members `digest`, `size`, `media`, and `name` (SPEC §15.1).
  `storage` is a storage name, which the root client binds to a storage class
  (SPEC §15.3).
- **Key-eligible** (SPEC Annex A.5): every type above except `Object` and
  `Blob`, their refinements, and objects of required key-eligible members.

### 1.1 Operators

Each operator has one overload for each type that it accepts. An `Int` operand
beside a `Decimal` one is converted to `Decimal`.

| Overload | Description |
| --- | --- |
| `fn + for Int(b: Int): Int`, `fn + for Decimal(b: Decimal): Decimal` | The sum. |
| `fn - for Int(b: Int): Int`, `fn - for Decimal(b: Decimal): Decimal` | The difference. |
| `fn * for Int(b: Int): Int`, `fn * for Decimal(b: Decimal): Decimal` | The product. |
| `fn / for Int(b: Int): Int` | The quotient, truncated toward zero (SPEC Annex A.3). |
| `fn / for Decimal(b: Decimal): Decimal` | The quotient (SPEC Annex A.3). |
| `fn % for Int(b: Int): Int`, `fn % for Decimal(b: Decimal): Decimal` | The remainder. |
| `fn - for Int(): Int`, `fn - for Decimal(): Decimal`, `fn - for Duration(): Duration` | The negation. |
| `fn ! for Bool(): Bool` | The negation. |
| `fn + for Text(b: Text): Text` | The concatenation. |
| `fn + for Timestamp(b: Duration): Timestamp`, `fn - for Timestamp(b: Duration): Timestamp` | The instant `b` later or earlier, exact in microseconds. |
| `fn - for Timestamp(b: Timestamp): Duration` | The time elapsed since `b`. |
| `fn + for Duration(b: Duration): Duration`, `fn - for Duration(b: Duration): Duration` | The sum and the difference. |
| `fn == for T(b: T): Bool`, `fn != for T(b: T): Bool` | Equality by value (LANGUAGE 6.3). |
| `fn < for T(b: T): Bool`, `fn <= for T(b: T): Bool`, `fn > for T(b: T): Bool`, `fn >= for T(b: T): Bool` | The order of SPEC Annex B. |
| `fn in for T(b: Table): Bool` | `b` has a row or an element equal to the value (LANGUAGE 6.3): `false` over an empty `b`, and `none` for an absent value over a non-empty `b`. |
| `fn + for Table(b: Table): Table` | `a + b`: the rows of `a` and a new row for each record or element of `b`. |
| `fn - for Table(b: Table): Table` | `a - b` is `difference(a, b)` (§4). |
| `fn \| for Table(b: Table): Table` | `a \| b` is `union(a, b)` (§4). |
| `fn & for Table(b: Table): Table` | `a & b` is `intersection(a, b)` (§4). |

---

## 2. Templates

| Signature | Description |
| --- | --- |
| `Table(element = none) { row }` | A stored collection of rows, each owner having its own table (SPEC §4.2). |
| `Set(of: type)` | Unique present values. |
| `Map(key: type, value: type)` | Unique present keys, each with one present value. |
| `Enum(labels: Set(of: Text))` | A `Text` restricted to `labels`. |
| `Module(interface: I)` | A mount point: a member that holds one child module satisfying `I` (SPEC §12.3). |

### 2.1 `Table`

- The body declares the members of each row, and every row has an identity
  (SPEC §4.2).
- `element:` is one member or a tuple of members that cover every stored
  member. A row is then also its **element**, the member's value or the tuple
  of values, and the table is a value: the collection of its elements.
- The slot `table { … }` holds views and mutations of the whole table: `.` is
  the table, `^` its owner. They are read and called through the table:
  `.tasks.total`, `.tasks.add(title: "x")`.
- Offers `unique` to the row body.
- `T?` on a table: absent is distinct from empty. `= none` removes every row
  and leaves it absent.
- An initializer or an inserted record supplies every required member that has
  no initializer; an omitted nested table starts empty. A table's initializer is
  **seed data** (SPEC §8.1).
- In the type of a parameter, a result, or an interface signature,
  `Table { … }` is a **shape**: the members of each row (LANGUAGE 12.3).
  `Table` alone takes any collection.

### 2.2 `Module`

- Holds a **module value**, owned by one place. `?` allows the mount point to
  be empty. A mount point that genesis would fill is optional.
- The body holds at most one `provide(…)`.
- `.m.v` reads the interface view `v`, and `.m.f(args)` calls the interface
  mutation `f` (SPEC §12.6). A path through a mount point is read-only.
- `has(.m)` and `modules.update(.m, …)` take the mount point as a place.
- Assigning `none` to the mount point, or removing the row that holds it,
  unmounts the module (SPEC §18.2).

---

## 3. Mixins

| Signature | Description |
| --- | --- |
| `assert(condition: Bool, message: Text? = none)` | In a body: `condition` holds of every committed state, `.` being the body's current value. In a program: `condition` holds when the statement runs. In an input type: `condition` holds of the argument. `none` passes. An omitted message is generated from the condition. |
| `unique(members)` | In a table's row body: no two rows of one owner hold equal values of `members`, a member of the row or a tuple of members (SPEC §4.3). A row with an absent member takes no part. |
| `provide({ view name = e … })` | In a mount point's body: the views that the child reads as `#name` (LANGUAGE 12.7). `.` is the mount point, `^` its owner. |

- A failing `assert` rejects the transaction with the code `constraint` in a
  body or an input type, and `assertion` in a program; a violated `unique` with
  `uniqueness` (SPEC §20.3).
- A body `assert` is judged on the complete result of the transaction. It reads
  what a view reads.
- **Placement.** An implementation SHOULD warn when a body `assert`'s condition
  reads nothing through `.` or `^`, and name the body it belongs in.
- A selection whose equalities cover the members of one `unique` of a table
  read from its owner holds at most one row, and reads as an optional value
  where one value is expected: `.attributes[.key == "color"].value ?? "plain"`.
- `unique` members are stored fields or objects of a key-eligible type, or the
  optional type of one.

---

## 4. Mappings

A mapping takes the collection on the left of `|>` as its first argument
(LANGUAGE 7.4).

| Signature | Description |
| --- | --- |
| `filter(rows: Table, conditions: . -> Bool…): Table` | The rows for which every condition holds: the selection `rows[conditions…]`. |
| `sort(rows: Table, terms: . -> T…): Table` | The rows ordered by the terms, lexicographically. Absent values sort last; identity breaks ties. Replaces any earlier order. |
| `skip(rows: Table, n: Int): Table` | The rows after the first `n`, in the order of the `sort` before it. |
| `take(rows: Table, n: Int): Table` | The first `n` rows, in the order of the `sort` before it. |
| `map(rows: Table, f: . -> T): Table` | One row for each row: the value of `f`, with the row's identity. |
| `group(rows: Table, keys: . -> T…, f: . -> T): Table` | One row per distinct key, holding the keys and the members of the record `f`, with the key as identity. In `f`, `.` is the group: the rows of one key. |
| `union(rows: Table, other: Table, by: . -> T = .): Table` | The rows, and the rows of `other` whose `by` equals that of none of them. |
| `intersection(rows: Table, other: Table, by: . -> T = .): Table` | The rows whose `by` equals that of a row of `other`. |
| `difference(rows: Table, other: Table, by: . -> T = .): Table` | The rows whose `by` equals that of no row of `other`. |

- `filter`, `sort`, `skip`, `take`, `union`, `intersection`, and `difference`
  keep every row as it is.
- `n` is evaluated once, at the level of the call. A negative `n` rejects the
  evaluation.
- `group` keys are key-eligible, and a computed key is named:
  `group(.customer)`, `group(day: time.date(.at))`. Each member of `f`
  aggregates the group or reads a key.
- `by` is written at the level of each row of both operands, which share what
  it reads. It defaults to the whole row, so `a | b`, `a & b`, and `a - b` are
  `union(a, b)`, `intersection(a, b)`, and `difference(a, b)`.

---

## 5. Aggregates

| Signature | Description |
| --- | --- |
| `count(rows: Table): Int` | The number of rows. |
| `sum(values: Table): T` | The sum; `0` of `T` when empty. |
| `avg(values: Table): Decimal?` | The mean, by `numeric` division (SPEC Annex A.3); `none` when empty. |
| `min(values: Table): T?` | The least value; `none` when empty. |
| `max(values: Table): T?` | The greatest value; `none` when empty. |
| `distinct(values: Table): Set(of: T)` | The distinct values, key-eligible. |

Absent values are skipped. Over rows whose records have one member, an
aggregate reads that member: `sum(.invoices |> map { .total })` is
`sum(.invoices.total)`.

---

## 6. Functions and Mutations

| Signature | Class | Description |
| --- | --- | --- |
| `has(x: T): Bool` | immutable | `x` is present; a collection holds a row; a mount point holds a module. |
| `size(x: Text): Int`, `size(x: Bytes): Int` | immutable | The length in Unicode scalar values, or in bytes. |
| `round(x: Int, digits: Int = 0): Int`, `round(x: Decimal, digits: Int = 0): Decimal` | immutable | Rounded to `digits` fractional digits, halfway away from zero (SPEC Annex A.3). |
| `trunc(x: Int, digits: Int = 0): Int`, `trunc(x: Decimal, digits: Int = 0): Decimal` | immutable | Rounded toward zero to `digits` fractional digits. |
| `floor(x: Int)`, `floor(x: Decimal)` | immutable | The greatest integer at most `x`. |
| `ceil(x: Int)`, `ceil(x: Decimal)` | immutable | The least integer at least `x`. |
| `desc(x: T): T` | immutable | `x` in reverse order, as a `sort` term. |
| `now(): Timestamp` | stable | The instant of the transaction or reading operation (SPEC §19.2). |
| `uuid(): Uuid` | mutation | A fresh UUID. |

| Namespace | Class | Publishes |
| --- | --- | --- |
| `string` | immutable | Unicode text functions (SPEC §13.1). |
| `convert` | immutable | Checked conversions. |
| `hex`, `base64` | immutable | Conversions between `Bytes` and `Text`. |
| `sha` | immutable | Cryptographic hashes. |
| `object` | immutable | Typed reads of `Object` values. |
| `time` | immutable | Unix time, dates, durations, and time zones. |
| `random` | mutation | Random bytes. |
| `history` | mutation | `history.at(source, instant: Timestamp)`: rows of `source` as they stood at `instant`, as records `{ row, history }` (SPEC §16.12). |
| `modules` | mutation | `modules.mount(artifact: Blob)`: a module value from a package, assigned to a mount point. `modules.update(m, artifact: Blob)`: moves the module at `m` along its migration chain (SPEC §12.4). |

---

## 7. Metaproperties

| Metaproperty | Type | Description |
| --- | --- | --- |
| `$history` | `{ created_at: Timestamp; updated_at: Timestamp }` | Of every stored row: the commit instants of its insertion and of the latest change of its own stored fields (SPEC §16.12). |

---

## 8. Prelude

The prelude is a library, found last by name (LANGUAGE 12.5):

```liasse
public {
    /// Unique present values.
    template Set(of: type): Table(element: .value) {
        value: of
        unique(.value)
    }

    /// Unique present keys, each with one present value.
    template Map(key: type, value: type): Table(element: (.key, .value)) {
        key: key
        value: value
        unique(.key)
    }

    /// A `Text` restricted to `labels`.
    template Enum(labels: Set(of: Text)): Text {
        assert(. in labels, "Unknown label")
    }
}
```

`x in s` tests membership in a set, and `has(m[.key == k])` a key of a map.
