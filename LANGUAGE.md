# The Liasse Language

Language version `liasse 0.0.1`.

This document defines the Liasse language: the syntax of its source files and
what each form means. [BUILTINS.md](BUILTINS.md) defines the builtins.

A source file is one of three kinds:

| File | Holds |
| --- | --- |
| `name.liasse` | a module: its state, views, and mutations, or, for a library, the definitions it publishes (chapter 12) |
| `name.lib.liasse` | a library file: functions, templates, mixins, and interfaces of the module file beside it (12.1) |
| `from-to.migration.liasse` | a migration between two versions of a module (chapter 13) |

---

## 1. Notation

**MUST**, **MUST NOT**, **SHOULD**, and **MAY** are used as in BCP 14.

Annex A is the grammar of the language, written in this notation:

```text
Rule  = Alternative | Alternative      alternation
'x'                                    literal token
A B                                    sequence
A?   A*   A+                           optional, zero or more, one or more
(A B)                                  grouping
A % ','                                one or more A separated by ','
NAME  TEXT  INT  DECIMAL               tokens (chapter 2)
```

Every `liasse` block derives from Annex A: from `Module`, `LibraryFile`, or
`MigrationFile` when it shows a whole file, and otherwise from the entries of
a `Body`, the statements of a `Program`, or one `Expression`.

---

## 2. Source text

### 2.1 Entries

A source file is a list of entries, one per line or separated by `;`. The
simplest entry declares a data member with its value, and optionally its type:

```liasse
title = "Workspace"
retry_limit: Int = 3; archived = false
```

### 2.2 Tokens

```liasse
count = 42                          // INT
ratio = 0.25                        // DECIMAL
quote = "Say \"hi\"\n"              // TEXT
poem = """
    Several lines, with "quotes" as written.
    """
pattern = r"\d+\.\d+"               // raw TEXT
```

- A name is an ASCII letter followed by ASCII letters, digits, and `_`. Names
  are case-sensitive.
- `INT` and `DECIMAL` are exact. A `-` before a number is negation.
- A text escapes `\n`, `\r`, `\t`, `\\`, `\"`, and `\u{XXXX}`; `"""…"""` spans
  lines and holds quotes as written; `r"…"` holds its characters as written.
- Source is UTF-8. Whitespace other than newlines, and comments, separate
  tokens.

The reserved words are `let`, `return`, `mut`, `view`, `fn`, `slot`,
`template`, `mixin`, `public`, `interface`, `mount`, `use`, `in`, `none`,
`true`, and `false`. A name that an author chooses differs from every
reserved word. A member or argument name that a builtin or a requirement
fixes MAY be one, written after `.` or before `:`: `interface: Billing`,
`.session_keys.public`.

These names have a meaning only where the language admits them: `meta` and
`requires` at the root of a module file (12.1, 12.5); `as` in a `use` entry
(12.5); `type` in the kind of a parameter (8.2, 10.2); `old` in a migration
(13.6); and `add`, `remove`, `rename`, `retype`, `from`, `to`, `up`, `down`,
and `drop` in a migration file (13.2).

### 2.3 Comments and documentation

```liasse
//! Documentation of the file, before its first entry.

/// Documentation of `address`.
address: {
    //! Documentation of the object's body.
    city: Text          // a line comment
    /* a block comment /* nested */ */
}
```

`///` and `/** … */` document the declaration, parameter, member, or migration
entry that follows. `//!` and `/*! … */` document the enclosing body or file,
before its first entry.

### 2.4 Line breaks

A newline ends an entry, a member, or an expression. These newlines are
layout, and the line continues:

- a run of newlines, which counts as one;
- a newline directly before or after `,` or `;`;
- a newline directly after an opening bracket or directly before a closing
  one;
- a newline directly before a `{` that opens a body, a block, or the record of
  a mapping;
- a newline directly before a line that begins with `|>`;
- a newline directly after an operator of an expression, `|>` and `->`
  included;
- a newline directly before or after the `?` or the `:` of a conditional.

A value starts on the line of the `=`, `+=`, or `-=` before it. Entries are
separated by a newline or `;`; members of a record, an argument list, an array,
a block, `meta`, `requires`, and a retype mapping by a newline or `,`. A
trailing separator is allowed.

```liasse
total = 1 +
    2
status = .urgent
    ? "triage"
    : "open"
point = {
    x: 1
    y: 2
}
```

---

## 3. Values

### 3.1 Literals

```liasse
retries: Int = -7
ratio: Decimal = 3.50
city: Text = "Paris"
active: Bool = true
starts_on: Date = "2026-09-25"
timeout: Duration = "PT5M"
```

An integer, a decimal, a text, `true`, and `false` are literals, and `none` is
the absence of every optional type (4.2). A text literal where another type is
expected spells a value of that type, decoded when the module is built.

### 3.2 Records, tuples, and arrays

```liasse
origin = { city: "Paris", country: "FR" }   // a record
span = { low: 10, high: low + 5 }           // a member reads a sibling
pair = ("daily", 100)                       // a tuple
single = ("daily",)                         // a one-tuple
limits = [10, 20, 50]                       // an array
```

A record's members match by name, in any order, and a member MAY read a sibling
member by its bare name, acyclically. `(a)` is `a` in parentheses; `()` is the
empty tuple. An array's element type comes from its position.

---

## 4. Declarations

### 4.1 Data members

```liasse
title: Text                 // a member of type Text
retry_limit: Int = 3        // with an initializer
label = "Open"              // the initializer gives the type
```

A data member is stored state. Its initializer gives its first value, and
writes (chapter 9) then set any value of its type: `done = false` is a `Bool`
that a mutation sets to `true`. A value that a write or an insertion supplies,
`none` included, takes the place of the initializer. A required member of the
module root MUST have an initializer, or be built from the initializers of its
members.

### 4.2 Optional members

```liasse
nickname: Text?             // `none` when absent
due: Date? = none
```

A required member always holds a value. An optional member holds a value or
`none`.

### 4.3 Objects

```liasse
address: {
    street: Text
    city: Text
    country = "FR"
}
billing_address: { street: Text; city: Text }?
```

An object groups members under one name, `.address.city`, and lives and dies
with its owner. Its value is a record with exactly its members: every required
member present, every optional member present or `none`.

### 4.4 Definitions

What follows `:` is a **definition**:

```liasse
title: Text
tags: Set(of: Text)
contract: Blob(storage: "archive")?
tasks: Table {
    title: Text
    done = false
}
```

A definition is a type name, optionally qualified by a namespace (`ns.Name`),
optionally applied to named arguments, optionally followed by a body, and
optionally marked `?`. `Text? { … }` and `Text { … }?` are the same
definition, and a definition takes one `?` at most. A plain `{ … }` is an
object (4.3).

A name applied to arguments is a **template**: a named definition with
parameters. `Set(of: Text)` applies the template `Set` with `of` bound to
`Text`. A definition is evaluated when the module is built. Chapter 11
declares templates.

`tasks` is a **collection**: its value is rows, each with the members that its
body declares.

Arguments of a definition are named, and form one record: `Set(of: Text)` is
`Set({ of: Text })`, the members in any order. A defaulted parameter is
supplied or left out.

### 4.5 Bodies

```liasse
companies: Table {
    //! Each row is one company.
    name: Text
    offices: Table { code: Text; city: Text }
}
```

- Within one body, data members, views, mutations, and slots have distinct
  names; templates, mixins, functions, and interfaces have distinct
  names among themselves. A nested body MAY reuse a name of an outer one, and
  the nearer declaration shadows the outer.
- The order of entries matters for documentation and for where a slot or a
  mixin application (5.2) inlines content. Views, initializers, and defaults
  are evaluated in the order of what they read, and mutations in the order of
  the mutations they call; these dependencies MUST form acyclic graphs.
- The `{` of a body stands on the line of its head or on the next line.

### 4.6 Names

The builtins are predeclared. A template, mixin, function,
interface, `requires` entry, parameter, or local takes a name distinct from
every builtin's. A data member, a view, or a mutation MAY reuse a builtin's
name, since a member is always read through `.`, `^`, `/`, or `#`.

An unqualified name in a definition, an application, or a call denotes the
builtin of that name, else the nearest enclosing declaration of that name, else
what the module imports (12.5). A qualified name `ns.name` denotes what the
namespace `ns` publishes.

---

## 5. Expressions

### 5.1 Levels and paths

Every body is a **level**. At each level, `.` is the current value: the module
root at the root, each row in the body of a collection, the object in an
object's body, the member's value in a member's body. `^` is the current value
one level out, `^^` two levels out, and each further `^` one level more, as far
as the root: in a body n levels below the root, `^` stacks at most n times, and
n times it is the root. The `^`s of a path stand at its start.

```liasse
region = "EU"
companies: Table {
    name: Text
    country: Text
    offices: Table {
        city: Text
        name = .city                      // `.` is the office
        country = ^.country               // `^` is its company
        region = ^^.region                // `^^` is the root
    }
}
```

| Path | Denotes |
| --- | --- |
| `.` | the current value |
| `.name` | the member `name` of the current value |
| `^`, `^^`, `^^^`, `^.name` | the current value one, two, and three levels out, up to the root, and a member of it |
| `/name` | the member `name` of the module root |
| `name` | a lexical binding: a sibling record member, a local, a parameter |

`title` and `.title` are different names: a bare name is lexical, and a member
is read through `.`.

A path that reads one member `x` from where it starts, such as `.x`, `^.x`,
`^^.x`, `/x`, or `task.x`, is a **short path** named `x`, and so is a name `x`
alone. In a record, a short path is a member of its name:

```liasse
{ .title, ^.name, /region, task.id, due }
{ title: .title, name: ^.name, region: /region, id: task.id, due: due }   // the same record
```

The arguments of a definition are written at the level of its body, so in
`Table(element: .code) { code: Text }` the `.` is each row. Levels are fixed by
the source when the module is built. Content that a template, a mixin, or a slot
inlines takes the level of the body it lands in.

### 5.2 Calls

```liasse
offices: Table {
    code: Text { assert(size(.) <= 8, "Code too long") }   // `.` is the code
    unique(.code)                                          // `.` is the office
}
```

```liasse
round(.amount, 2)                       // a builtin function
round(.amount, digits: 2)               // a defaulted parameter by its label
string.lower(string.trim(.title))       // functions of the namespace `string`
```

`name(arguments)` is a **call**, and its position gives its meaning:

| Position | The call |
| --- | --- |
| an entry of a body | applies a **mixin**: it inlines the mixin's declarations there, as if they were written there |
| an expression | calls a **function** and yields its result |
| a mutation (chapter 9) | calls a function or a **mutation** |

A **function** is pure and immediate: it changes nothing, and it yields its
result when it is called. An immutable function's result depends only on its
arguments. A stable function's result is fixed within one transaction, and it
MAY read state.

A **mutation** is not pure: two calls with the same arguments MAY give
different results, and a call MAY change state. A mutation MAY be
asynchronous, and it takes effect at the end of its transaction. Chapter 9
declares mutations with `mut`.

- A view and a function body call functions.
- An initializer calls functions and the mutations that change no state.
- An argument of a definition calls immutable functions.

`ns.name(arguments)` calls or applies what the namespace `ns` publishes: a
builtin namespace such as `string`, or a requirement of the module (12.5).
Chapter 8 declares functions, and chapter 10 mixins.

A function, and a mutation that a builtin or a namespace publishes, take their
terms in the order of their parameters: `round(.amount, 2)`. A trailing
parameter that has a default is omitted, or given by its label after the
terms. A mixin, a template, a view, and a mutation declared with `mut` take
named arguments, which form one record (4.4), and a bare name is punned there
as a record member is (5.1): `f(title)` is `f(title: title)`. The parentheses
are always written.

After `:`, `Name(arguments)` applies a template (4.4). `path.name(arguments)`
reads the view `name` of the value at `path` with those arguments (7.2), or,
in a mutation, calls its mutation `name` (9.2).

### 5.3 Operators

Tightest first:

| Operators | |
| --- | --- |
| `.name` `.$name` `[…]` `(…)` | postfix |
| `!` `-` | prefix |
| `*` `/` `%` | |
| `+` `-` | |
| `<` `<=` `>` `>=` `in` | comparison |
| `==` `!=` | equality |
| `&&` | |
| `\|\|` | |
| `??` | fallback |
| `\|` `&` | union, intersection (6.4) |
| `? :` | conditional |
| `\|>` | mapping (7.4) |

Binary operators of one level associate left; `? :` associates right. A chain
that mixes `|` and `&` is parenthesized.

```liasse
has(.shipped_at) && .shipped_at > .promised_at
.nickname ?? .name
.done ? "closed" : "open"
(.balance ?? 0.0) + 1.0
```

`a ?? b` is `a` when `a` is present and `b` otherwise. `&&`, `||`, `? :`, and
`??` evaluate only the operands they select, so `b != 0 && a / b > 1` never
divides by zero. The two branches of `? :` have one type, and so do the two
operands of `??`, up to optionality.

Every other operator is a builtin function, with one overload for each type
that it accepts: `a + b` calls the `+` for the types of `a` and `b`, and `==`
compares by value (6.3). An optional operand of an arithmetic operator is first
coalesced with `??` or tested with `has`.

### 5.4 Initializers

An initializer is evaluated once, when its member comes into existence, and
gives the member its first value (4.1). In an insertion, `.` is the row being
inserted, with every supplied value and every initializer already resolved:

```liasse
currency = "EUR"
invoices: Table {
    amount: Decimal
    currency = ^.currency       // the root's currency at insertion
    balance = .amount           // the amount, until payments reduce it
}
```

The member keeps that value until a write changes it: a later change of the
root's `currency` leaves the `currency` of every existing invoice as it is. A
value that follows what it reads is a view (7.1).

An initializer reads `.`, `^` and the levels further out, `/`, constants, and
the parameters of an enclosing template or mixin.

---

## 6. Collections

### 6.1 Collections

```liasse
tasks: Table {
    title: Text
    done = false
}
labels: Set(of: Text)
limits: Map(key: Text, value: Int)
```

A collection is unordered, and each of its rows has an **identity** that stays
the same while the row exists. A collection's definition MAY give each row an
**element**, a value that stands for the row: a text for each row of `labels`,
a pair of a key and a value for each row of `limits`. Where a collection is
expected, a record, an element, or an array of either is a collection of rows,
and `none` the empty collection.

```liasse
usage: Table {
    amount: Decimal
    funding: Table { pool: Uuid; quantity: Decimal }
}
```

A member read through a collection maps over its rows: `.usage.amount` is the
collection of the amounts, and `.usage.funding` the collection of every
funding row of every usage row, each keeping the identity of its owners.

### 6.2 Selection

```liasse
.tasks[.done]                                   // the rows whose `done` holds
.tasks[.owner == me, !.done]                    // `,` joins conditions
.tasks[.owner == me && !.done]                  // the same selection
.tasks[(.owner == me && !.done) || .urgent]
.tax_rates[.country == ^.country, .kind == ^.kind]   // `^` is the value outside
.tax_rates[^.country, ^.kind]                   // the same selection
.events[.at >= since, .at < until]
.tasks[id]                                      // `.id == id`
.tasks[task.id]                                 // `.id == task.id`
.offices[/region]                               // `.region == /region`
.people[.id in [lead, manager]]
.orders[has(.lines[.sku == sku])]               // the orders that have such a line
.people[!has(.email)]                           // the people without an email
```

`rows[c₁, c₂, …]` is the relational selection σ: the rows of `rows` for which
every condition holds. `,` joins conditions and binds loosest, so
`[a, b || c]` is `a && (b || c)`. A selection applies to any collection: a
member, a view, a local, the result of a mapping.

The brackets open a level: `.` is the row being tested, and `^` the value
outside the brackets. Lexical names keep their meaning. A condition that is
exactly a short path named `x` (5.1), other than `.x`, is the equality of `.x`
with it: `.x == x`, `.x == task.x`, `.x == ^.x`, `.x == /x`.

An absent member makes a comparison `none`, and a selection keeps only the
rows whose conditions hold, so `.tasks[!(.status in ["closed"])]` leaves out a
task whose `status` is absent.

### 6.3 Equality

```liasse
task == other               // equal values
"urgent" in .labels         // set membership
.status in ["open", "held"]
```

Values compare by value: two values are equal when they have the same
structure and equal data. Records and objects are equal when they have the
same members, each equal; tuples and arrays when they hold equal elements in
the same order; collections when each row of one pairs with an equal row of
the other; rows when their values, the records of their data members, are
equal, whatever their identities.

### 6.4 Combining collections

```liasse
.overdue | .urgent          // union: the left's rows, and the right's rows equal to none of them
.overdue & .assigned        // intersection: the left's rows equal to a row of the right
.open - .assigned           // difference: the left's rows equal to no row of the right
.mine ?? .shared            // the right one when the left is empty
.admin ? .tasks : []        // a conditional; `[]` is the empty collection
```

The operands have one row type, and their rows compare by value (6.3). `|`,
`&`, and `-` keep each row as it is.

### 6.5 Metaproperties

```liasse
.$history.updated_at
task.$history.created_at
```

`x.$name` reads the metaproperty `name` of `x`, and `.$name` that of the
current value. A metaproperty is read-only, and it stands beside the members:
a value and a record built from it hold the members, and a record holds a
metaproperty by naming it, as in
`{ .title, changed_at: .$history.updated_at }`. It maps over a collection as a
member does, and it is `none` through an absent value.

---

## 7. Views

### 7.1 Views

```liasse
view label = .done ? "Complete" : "Open"
view label_text: Text = .done ? "Complete" : "Open"
view open_tasks = .tasks[!.done]
```

A view defines a query. It is read like a data member, `.label`, and declared
where a data member is; declared in the body of a collection, it is a member of
each row. Reading a view evaluates its expression where the view is declared,
over the current state. A view's type is the type of its expression, and an
annotation such as `: Text` states it. A record and a row's value hold data
members only.

A view that yields rows keeps the identity of its source rows.

### 7.2 Parameters

```liasse
view by_owner(owner: Text) = .tasks[.owner == owner]
view by_status() = .tasks[.status == @status]
view task() = .tasks[@id]                   // `.id == @id`
view page(limit: Int = 20, after: Int = 0) = .tasks[.position > after, .position <= after + limit]
```

A view with parameters is read with named arguments: `.by_owner(owner: "ana")`.
A parameter is declared in the header, or by reading `@name` in the
expression, or both; `@name` and `name` are the same binding when the header
declares `name`. A parameter declared by use takes its type from the typed
positions it stands in, and every one of them gives the same type:

| Position | Gives |
| --- | --- |
| a selection condition: `[.at < @until]`, `[@id]` | the type of the row's side |
| a comparison or arithmetic with a typed operand: `.owner == @owner` | that operand's type |
| an argument of a function with one accepted type: `.tasks |> take(@n)` | that type |
| a named argument: `.page(limit: @limit)` | the parameter's type |

An argument of a function that accepts several types, and a record member such
as `{ x: @x }`, give none; a parameter used only there is declared in the
header. A default is an expression over constants and the other parameters.
The parameters, their types, and their defaults are part of the view's
contract.

`@name` is read in a view with parameters and in the selections and mappings
inside it.

### 7.3 Constants

```liasse
view labels = { open: "Open", complete: "Complete" }
view limit = 100
```

A view whose expression reads only literals and other constants is a
**constant**. Its value is fixed by the definition, read-only through every
path, and changed by a new version of the definition. A constant stands
wherever a build-time value is required, such as a template argument.

### 7.4 Mappings

```liasse
view open_tasks = .tasks
    |> filter(!.done)
    |> sort(.title, .id)
    |> map { .id, .title }
```

`x |> f(a, …)` is the call `f(x, a, …)`: the value on the left is the first
argument. A **mapping** is a function whose first parameter is a collection,
and a chain of mappings is a **mapping pipeline**: each mapping reads the
collection that the one before it yields. A scalar or a single row piped into
a mapping is a collection of one row.

A view's rows are in the order that its mappings give them, as
`sort(.title, .id)` orders `open_tasks`.

A parameter that is a function of each row, such as the condition of `filter`
or the record of `map`, takes `. -> e`: `.` is the row, and `^` the current
value where the argument is written. `e` alone means `. -> e`. Lexical names and
`@name` keep their meaning. A record after the arguments is the last argument,
and in an expression, braces always hold a record. These four are one mapping:

```liasse
.tasks |> map { .id, .title }
.tasks |> map { id: .id, title: .title }
.tasks |> map({ .id, .title })
.tasks |> map(. -> { .id, .title })
```

Every other argument is evaluated at the level of the call: in
`.tasks |> take(.page_size)`, `.page_size` is a member of the current value
where the call is written.

The order of the mappings defines the result. Each mapping evaluates only the
rows that the one before it yields:

```liasse
.tasks |> sort(.id) |> take(2) |> filter(!.done)   // at most two rows, then filtered
.tasks |> sort(.id) |> filter(!.done) |> take(2)   // filtered, then at most two rows
```

`rows |> filter(c)` and the selection `rows[c]` yield the same rows; `filter`
continues a pipeline.

### 7.5 Aggregates

```liasse
view open_count = count(.tasks[!.done])
view total = sum(.orders.amount)
view revenue = .invoices |> map { .total } |> sum
view largest = max(.teams |> map { size: count(.members) })
```

An **aggregate** is a function that takes a collection and yields one value. It
is called as every function is, or ends a pipeline:
`.invoices |> map { .total } |> sum` is `sum(.invoices |> map { .total })`. Its
argument is written at the level of the call, so `.` keeps its binding.

---

## 8. Functions

### 8.1 Declaring functions

```liasse
fn slug(title: Text): Text = string.lower(string.trim(title))

fn label(title: Text, fallback: Text = "Untitled"): Text {
    let t = string.trim(title)
    fn blank(s: Text): Bool = size(s) == 0
    return blank(t) ? fallback : t
}

tasks: Table { title: Text; view heading = label(.title) }
```

`fn name(parameters): T = e` yields the value of `e`. A block holds `let`
bindings and local functions, in order, and ends with one `return`. The result
type is written, so a brace after it opens the block. A function is declared at
the module root, directly in the body of a template or a mixin, or in a
function block, where its scope is the rest of the block.

A body reads its parameters, its locals, the functions and namespaces in scope,
`/` in a module, and the lexical bindings where the function is declared. The
caller's `.`, `^`, and `@name` reach it as arguments, as `.title` does in
`heading`. Functions call each other along an acyclic graph, local functions
and defaults included.

### 8.2 Parameters

```liasse
fn clamp(x: Decimal, low: Decimal = 0.0, high: Decimal = 1.0): Decimal = x < low ? low : (x > high ? high : x)
fn either(T: type, first: Bool, a: T, b: T): T = first ? a : b
fn open_count(tasks: Table { done: Bool }): Int = count(tasks[!.done])
fn count_where(tasks: Table { done: Bool }, keep: . -> Bool): Int = count(tasks[keep(.)])
view open = count_where(.tasks, !.done)        // `keep` is `. -> !.done`
```

A parameter's kind is a type `T`, for an expression of type `T`; `type`, for a
type, which makes the function generic; or `. -> T`, for a function of one
value, of result type `T`. An argument for `. -> T` is written as a mapping's
is (7.4), `. -> e` or `e` alone, and the body applies it to a value:
`keep(.)`. A collection definition in a parameter's type is a **shape**: the
parameter `tasks` takes any collection whose rows have a `done` of type
`Bool`. Only trailing parameters have defaults, evaluated for each call that
leaves them out.

### 8.3 Immutable and stable functions

A function declared with `fn` is stable when its body or its defaults read
state or call a stable function, and immutable otherwise (5.2). A call to it is
admitted in a position when its body, with its arguments substituted, is
admitted there.

---

## 9. Mutations

### 9.1 Mutations

```liasse
tasks: Table {
    title: Text
    done = false
    mut rename(title: Text) { .title = title }
    mut complete() { .done = true }
}
```

A mutation declared with `mut` is a program: statements that run in order and
change state as one **transaction**, which takes effect at its end, whole, or
leaves state as it was. It is declared where a data member is, and `.` in it
is the current value of its body: the row in a collection's body, the object
in an object's body, the root at the root. A program reads state, calls
functions and mutations, and writes.

### 9.2 Statements

| Statement | Meaning |
| --- | --- |
| `let x = e` | binds `x` from the next statement to the end of the program, once, under a name distinct from every input |
| `path = e` | the place at `path` takes the value of `e`: a member, on every row when the path crosses a collection, `= none` clearing an optional one; an object; a collection, whose rows become those of `e`, a collection or an array of records or elements |
| `rows[conditions] = r` | **put**: every row that the equalities match becomes `r`, or `r` is inserted when none matches |
| `path += e`, `path -= e` | `path = path + e` and `path = path - e`, by the operators of the place's type |
| `target { patch }` | assigns several members of the selected rows or object |
| `path.f(args)` | calls a mutation |
| `f(args)` | calls a builtin mutation or one that a namespace publishes (5.2), or runs `assert` |
| `return e` | the result; the last statement |

```liasse
mut add_task(title: Text) {
    let task = .tasks += { title }
    return task |> map { .id, .title, .done }
}
```

Statements run in source order, each over the state that the statements before
it leave. A write, and a call of a mutation that changes state, stand as a
statement, or as the whole expression of a `let` or a `return`. `rows += e`
yields the rows it inserts, `rows = e` the rows of `e`, a put the rows it
writes, `rows -= e` the rows it removes as they stood before, and a mutation
call its result. A program holds at least one statement. The result
type is the annotation `mut f(…): T`, or the type of the `return` expression.

**Collections.** `rows += e` inserts a new row for each record or element of
`e`, which is a record, an element, a collection or an array of either, or
`none` for nothing. `rows -= e` removes the rows equal to a row or an element
of `e` (6.3). The rows nested in a removed row go with it.

```liasse
mut purge_archived() { .tasks -= .tasks[.archived] }
mut untag(id: Uuid, tag: Text) { .tasks[id].tags -= tag }
```

**Put.** `rows[conditions] = r` inserts or replaces. Its conditions are
equalities `.m == v`, or their shorthands, on distinct stored members; `r` is a
record or an element, and repeats a condition member only with an equal value.

```liasse
mut tag_once(id: Uuid, tag: Text) { .tasks[id].tags[.value == tag] = {} }   // adds `tag`, or changes nothing
mut set_limit(key: Text, value: Int) { .limits[.key == key] = { value } }    // sets the entry
```

**Patch.** A block after a target assigns several members, each right-hand side
reading the state at the start of the patch. `=path` assigns the member named
by the path's last name, and a nested block patches an object member.

```liasse
mut update(id: Uuid, name: Text, locale: Text) {
    .people[id] { =name, email = none, profile { =locale } }
}
```

**Targets.** A write reaches stored rows through a path, a selection, or a
mapping that keeps rows. A selection writes exactly one row; a filtered
collection writes every row it selects, and none when it selects none.
One statement writes each row once. A required member takes a present value:
an optional one is coalesced with `??`.

```liasse
mut archive_done() { (.tasks |> filter(.done)).archived = true }
```

**Mutation calls.** `path.f(args)` runs the mutation `f` that the body of the
path's value declares, with `.` bound to that value: the selected row, every
row of a filtered collection in turn, an object, or `/` for the root. It runs
inside its caller's transaction, and mutations call each other
along an acyclic graph. A block after a mutation call patches the call's
result, a row or an object.

### 9.3 Inputs

```liasse
mut rename(title: Text) { .title = title }     // declared
mut rename() { .title = @title }               // declared by use
mut rename(title) { .title = title }           // declared, type from the body
```

All three declare one required `Text` input named `title`. Inputs are values:
each argument is evaluated once, where the mutation is called. They are
declared as a view's parameters are (7.2), and a program adds typed positions:

| Position | Gives |
| --- | --- |
| an assignment or a patch: `.title = @title`, `{ =@title }` | the member's type and optionality |
| a member of an inserted or put record: `.data += { @value }` | the row member's type and optionality |
| a member of an element: `.limits += (@name, @amount)` | that member's type |
| an object assignment: `.address = @address` | the object's type |
| `let x = @value` | the positions of `x` |

`return @x` and the whole operand of `+=` or `-=`, as in `.tags += @tag`,
accept several types and give none.

```liasse
mut schedule(title: Text, at: Timestamp = now(), note: Text? = none) {
    .events += { title, at, note }
}
```

- A default reads the other inputs by their bare names and constants, and calls
  functions and the mutations that change no state. Defaults that read each
  other form no cycle.
- An input of optional type is omissible, and omission is `none`; with a
  default, omission gives the default. Every other input without a default is
  required. An inferred input that needs a default is declared in the header.
- `@name` is read in a program and in the selections and mappings inside it. A
  mutation that it calls receives only the arguments that its call passes.
- A mutation is called with named arguments. `{ @value }` is
  `{ value: @value }`, `{ =@value }` is `value = @value`, and `.f(@value)` is
  `.f(value: @value)`.

An input's type is a named type such as `Text` or `Timestamp`, an object, or a
collection of elements, each optionally `?`, or a template application whose
definition is one of these. A body on the type judges each value bound to the
input:

```liasse
mut retitle(title: Text { assert(size(.) > 0, "A title is required") }) { .title = title }
```

The arguments are parsed against the inputs before the first statement runs.
The **contract** of a mutation is its inputs' names,
types, optionality, and checks, and its result.

### 9.4 Examples

```liasse
data: Table {
    id = uuid()
    unique(.id)
    value: Decimal
    note: Text? = none
    mut set_note(note: Text?) { .note = note }
}

mut store() {
    .data += { @value, @note }
    .data[@id] { =@value, =@note }
    .data[@id].set_note(@note)
}
```

`store` has the inputs `id: Uuid`, `value: Decimal`, and `note: Text?`; `note`
is omissible, and omitting it stores `none`.

```liasse
mut withdraw(amount: Decimal) {
    assert(.balance >= amount, "Insufficient funds")
    .balance -= amount
}

mut import_people() { .people += (.staged_people |> map { .id, .name, .email }) }
mut replace_people() { .people = .staged_people |> map { .id, .name, .email } }

mut restore_title(id: Uuid, at: Timestamp) {
    let past = history.at(.tasks[id], at)
    assert(has(past), "The task has no version at that instant")
    .tasks[id].title = past.row.title
}
```

---

## 10. Mixins

### 10.1 Declaring mixins

```liasse
mixin archivable {
    archived = false
    mut archive() { .archived = true }
}

tasks: Table {
    archivable()
    title: Text
}
```

A mixin is a named fragment of declarations. An application inlines them at
its position, with the arguments substituted and its own applications
inlined in turn, so `tasks` has the members `archived` and `archive`. Each
application declares its own members. A mixin is declared at the module root,
in an object or row body, or in a template or mixin body.

### 10.2 Parameters

```liasse
mixin labelled(field: Text, message: Text = "A label is required") {
    view label = string.trim(field)
    assert(size(field) > 0, message)
}

documents: Table {
    title: Text
    labelled(field: .title)
}
```

A mixin declared in the language takes named arguments (4.4). A parameter's
kind is one of:

| Kind | Argument |
| --- | --- |
| a type `T` | an expression of type `T` |
| `type` | a definition |
| `. -> T` | a function of one value, of result type `T` (8.2) |
| left out | an expression of any type |

An argument is written at the level of the body that applies the mixin, and
evaluated where the mixin reads its parameter, with the levels and names of
the place where it is written: in `documents`, `field` is the title of
each row. Where a use needs a build-time value, such as the argument of a
definition, the argument is a constant or an expression over constants that
calls immutable functions. A default is an expression over the other
parameters and constants, and the defaults are acyclic.

### 10.3 The current value in a mixin

A mixin's declarations take the level of the body that applies it: `.` is its
current value, each row in a collection's body, the object in an object's
body, the value in a member's body, and the root at the root; `^` is one level
out.
Through `.`, a mixin's declarations read the members that its expansion
declares; a mixin reads a member of its host through an argument, as
`labelled` reads `.title`. The names in a mixin's declarations resolve where
the mixin is declared.

A name that a mixin's expansion and its host both declare, or that two
applications in one body both declare, is one name declared twice. A mixin whose
expansion holds only unnamed entries, such as `assert` applications, is applied
any number of times. Mixins apply each other along an acyclic graph. An
application stands where every declaration of its expansion stands.

```liasse
mixin not_blank(message: Text = "Required") {
    assert(size(string.trim(.)) > 0, message)
}

people: Table {
    name: Text { not_blank() }
    city: Text { not_blank(message: "A city is required") }
}
```

---

## 11. Templates

### 11.1 Declaring templates

```liasse
template Task: Table {
    title: Text
    done = false
    mut complete() { .done = true }
}

tasks: Task
archived: Task
```

A template is a named definition with parameters. Applying it yields its
definition with the arguments substituted and the slots filled, and that
definition is elaborated like any other, templates it applies included. `tasks`
and `archived` are two independent collections: each application owns its own
state, identities, and authority, and its members sit at the application's path.
A template is declared where a mixin is, and its definition's body is an object
or row body.

### 11.2 Parameters

```liasse
template Entries(Value: type, title_limit: Int = 200): Table {
    title: Text { assert(size(.) <= title_limit, "Title too long") }
    payload: Value
}

notes: Entries(Value: Text, title_limit: 120)
counts: Entries(Value: Int)
```

```liasse
template Keyed(by): Table { unique(by); slot }

offices: Keyed(by: (.country, .code)) {
    country: Text
    code: Text
}
```

A template's parameters have the kinds of a mixin's (10.2) and take named
arguments, written at the level of the definition's body (5.1): `(.country,
.code)` reads the members of each row of `offices`. A parameter is a lexical
binding inside the template's definition, and every name in the definition
resolves where the template is declared.

### 11.3 Slots

A slot marks where a template inlines the content that its application
supplies.

```liasse
template Task: Table {
    title: Text
    done = false
    slot
}

tasks: Task {
    assignee: Text?
    assert(!.done || has(.assignee), "A completed task names its assignee")
}
```

```liasse
template Card: {
    title: Text
    slot notes { memo: Text? }
}

plain: Card                 // `notes` holds its fallback, `memo`
bare: Card { notes { } }    // `notes` is empty
```

```liasse
template Ledger: Table {
    entries: Table {
        amount: Decimal
        slot entry
    }
    slot
}

payments: Ledger {
    label: Text
    entry { assert(.amount <= 1000.0, "One payment cannot exceed 1000") }
}
```

- `slot` is the default slot, and `slot name` a named slot. A definition has
  at most one default slot and one slot of each name. The nearest enclosing
  template or mixin owns a slot, at any depth.
- The bare entries of an application's body fill the default slot, and a block
  `name { … }` fills the slot `name` wherever it is declared.
- A slot with a body has fallback content, used when the application leaves the
  slot unfilled. A present empty block discards the fallback. Supplied content
  takes the fallback's place as a whole.
- Slot content resolves its names where the application is written. It takes
  the level where the slot lands: `.` is each row in a collection's body and the
  value in a member's body, and `^` is one level out. Through `.` it reads
  every member of that value, the template's members included.
- Slot content adds declarations: their names differ from the template's in
  the body where they land, the template's checks hold, and the template's
  parameters stay inside its definition.

What each slot binds, and the templates and mixins it offers (11.4), are part
of the template's contract.

### 11.4 Offered templates and mixins

```liasse
template Board: Table {
    name: Text
    template Card: Table {
        title: Text
        view board = ^.name                 // `^` is the board row
        slot
    }
    mixin pinnable { pinned = false; mut pin() { .pinned = true } }
    slot
}

boards: Board {
    cards: Card { pinnable() }
    archive: Card
}
```

A template or a mixin declared directly in a template's body is offered to the
content of every slot of that template and to the template's own
declarations; one declared in a slot's body is offered to that slot's content
and fallback. An offered template or mixin is applied by its bare name, and it
joins the names that slot content resolves: a clash with a name visible where
the application is written is one name declared twice.

An offered template or mixin resolves its names where it is declared, the
template's parameters included, and it reaches the levels of the template that
declares it through `^`, as a declaration at the root reaches the root through
`/`: in `Card`, `^` is the board row.

A component offers mixins in the slots of the types and templates it publishes,
and of the builtin ones. They are applied in a module that requires the
component, and each application uses the requirement (12.5). Two components that
a module requires and that offer one name in one slot make that name ambiguous
there.

### 11.5 Mixins with slots

```liasse
mixin priced(currency: Text) {
    amount: Decimal
    currency = currency
    slot
}

orders: Table {
    priced(currency: "EUR") { tax: Decimal }
}
```

A mixin declares slots as a template does, and the body of its application
fills them.

### 11.6 Recursion

```liasse
template Company: Table {
    name: Text
    subcompanies: Company
}
```

A template refers to itself by name. Its values are finite: an empty collection
or an absent optional member ends the recursion, and its applications form a
finite graph.

---

## 12. Modules

### 12.1 Module files

```liasse
//! A task list.
meta { name: "example.tasks", version: "1.0.0", language: "liasse 0.0.1" }

tasks: Table {
    title: Text
    done = false
}
```

A module file holds one module, whose root is a body. It holds the current
version of the module; earlier versions and the migrations between them are
separate files (chapter 13).

`meta` opens every module file and has exactly the members `name`, `version`,
and `language`, each a text:

- `name` is one or more components separated by `.`, each a lower-case ASCII
  letter followed by lower-case ASCII letters, digits, and `_`, such as
  `"acme.tasks"`;
- `version` is three base-10 integers separated by `.`, each `0` or starting
  with a digit from 1 to 9, such as `"1.10.0"`, ordered by major, minor, and
  patch, numerically;
- `language` is `"liasse <version>"`.

`name@version` is the module's identity, here
`example.tasks@1.0.0`.

A **library file**, `name.lib.liasse`, lies beside a module file and holds
functions, templates, mixins, and interfaces of that module's root, as if they
were written in the module file. The module file does not declare it: every
library file beside it is part of the module, and the names of both resolve
at the root. A library file has no `meta`; the module file's identifies both.
A library file of `tasks.liasse`:

```liasse
//! Limits of task fields.
mixin short(limit: Int = 200) { assert(size(.) <= limit, "Too long") }
```

`tasks.liasse` applies it as a mixin of its own: `title: Text { short() }`.

### 12.2 Public members

```liasse
public {
    view open_tasks = .tasks[!.done]
    mut add(title: Text) { .tasks += { title } }
}
```

`public { … }` holds what a module publishes. It stands at most once, at the
root of the module file. It holds views, mutations, and mixin applications
that expand to them. Its members are members of the root, `.open_tasks`, and
`.` in it is the root.

### 12.3 Interfaces

```liasse
interface Billing {
    view invoices: Table { number: Text; total: Decimal }
    mut charge(amount: Decimal)
}
```

An interface is a contract of signatures: `view name: Type`, and
`mut name(parameters): Result` with an optional result. An interface is
declared at the root of a module, or written inline where one is expected, as
a body of signatures.

In a signature and in a mutation's result, a collection definition is a shape
(8.2): the members of each row. A result presents exactly the shape's members. A
value **satisfies** an interface through its members: the root members of a
module, whatever `public` holds, or the members of a row or an object. For
each `view v: T`, it has a data member or a view `v` whose type conforms to
`T`, a collection conforming when its rows have every member of the shape, each
of a conforming type; for each mutation signature, a mutation of that name
with exactly the signature's inputs, of the same names, types, and
optionality, and a result that conforms. Through an interface, only the
shape's members are read.

### 12.4 Libraries

A library:

```liasse
//! Text helpers.
meta { name: "acme.text", version: "1.2.0", language: "liasse 0.0.1" }

fn blank(t: Text): Bool = size(string.trim(t)) == 0

public {
    fn slugify(t: Text): Text = string.lower(string.trim(t))
    template Slug: Text { assert(!blank(.), "A slug is not empty") }
    mixin not_blank(message: Text = "Required") { assert(!blank(.), message) }
    interface Named { view name: Text }
}
```

A library is a module without state, which other modules require (12.5). Its
module file and library files hold functions, templates, mixins, and
interfaces, with a `meta`, at most one `requires`, and `use` entries. Its
`public { … }` holds the definitions that it publishes, and the others, such
as `blank`, serve the library itself. The names in a library resolve in the
library, and its definitions receive a module's state through their
parameters.

### 12.5 Requirements and imports

```liasse
meta { name: "acme.tasks", version: "1.0.0", language: "liasse 0.0.1" }
requires { text: "acme.text@1" }
use text.slugify
use text.Slug as TaskSlug

tasks: Table {
    title: Text { text.not_blank() }
    slug: TaskSlug = slugify(.title)
}
```

A `requires` entry binds a local name to a library or a component contract,
by name and major version. It names a library when `children/` holds a library
of that name and major (13.1), and a component contract otherwise. The local
name differs from the builtin namespaces and the root
members. What a requirement publishes is named through its local name,
`text.not_blank`, or brought unqualified by `use`. A library's requirements
are met by every module that requires it.

```liasse
use text.slugify
use text.slugify as slug
use text.{ slugify, Slug as TaskSlug }
use string.*
```

`use ns.name` brings one name that a namespace publishes, and
`use ns.name as local` brings it under the name `local`. `use ns.{ … }` brings
the listed names, each with its own `as`, and `use ns.*` every published name
except the builtins'. The namespace is a requirement or a builtin namespace.
`use` entries stand at the root of a module file, after its `requires`, and
act in the module, its library files included (12.1). A name that `use`
brings denotes what its qualified form denotes.

An unqualified name resolves, first to last, to a builtin; to the nearest
enclosing declaration of that name, where the root counts its declarations and
the names of its explicit `use` entries; to what a `use ns.*` brings; and to
the builtin prelude. A name that an explicit `use` brings is distinct from
the root's declarations, the other explicit `use` names, and the builtins; a
name that two `use ns.*` bring from different definitions is written qualified.

An implementation SHOULD warn about an unused requirement. A requirement is used
by a qualified name, by a name that `use` brings, by an application of a mixin
that it offers (11.4), and, for a component, by a required library that
requires it.

### 12.6 Child modules

```liasse
interface Personal {
    view profile: { name: Text; email: Text }
    mut set_email(email: Text)
}

users: Table {
    id = uuid()
    unique(.id)
    data: Module(interface: Personal)?
    mut join(artifact: Blob) { .data = modules.mount(artifact) }
    mut change_email(email: Text) { .data.set_email(email) }
    view name = .data.profile.name
}
view names = .users.data.profile.name
```

A member such as `data` is a **mount point**: it holds a child module. Paths
reach the child through its interface: `.data.profile` reads its view
`profile`, `.data.set_email(email)` calls its mutation `set_email`, and
`.users.data.profile.name` reads `profile.name` through every user whose mount
point holds a module.

### 12.7 Provided context

```liasse
//! The personal data of one user.
meta { name: "acme.personal", version: "1.0.0", language: "liasse 0.0.1" }
mount user { view id: Uuid; view region: Text }

profile: { name: Text = ""; email: Text = "" }
view home = #user.region
mut set_email(email: Text) { .profile.email = email }
```

```liasse
users: Table {
    id = uuid()
    unique(.id)
    region: Text
    data: Module(interface: Personal) {
        provide({ view id = ^.id; view region = ^.region })
    }?
}
```

`mount name: I`, or `mount name { signatures }`, at a module's root declares
the context that the module needs from its parent: an interface of view
signatures. The module reads it as `#name`, wherever it reads `/`:
`#user.region` evaluates the view `region` that the parent provides in the
body of the mount point, here with `provide`, where `^` is the user row. A
module declares at most one `mount`, and a module that declares one is mounted
as a child.

### 12.8 Sessions

```liasse
interface Member {
    view my_tasks: Table { id: Uuid; title: Text }
    mut rename_task(id: Uuid, title: Text)
}

tasks: Table {
    id = uuid()
    unique(.id)
    owner: Uuid
    title: Text
}

accounts: Table {
    id = uuid()
    unique(.id)
    code: Uuid = uuid()
    unique(.code)
    name: Text
    view my_tasks = /tasks[.owner == ^.id] |> map { .id, .title }
    mut rename_task(id: Uuid, title: Text) { /tasks[id, .owner == ^.id].title = title }
}

public {
    mut sign_in(code: Uuid): Member { return .accounts[code] }
}
```

A mutation whose result type is an interface returns a **session**: the value
of its `return`, a row, an object, or the root, reached through the interface
that it satisfies (12.3). The interface's views read, and its mutations run,
with `.` bound to that value: `rename_task` renames the tasks of the account
that signed in. A session is one value, so `.accounts[code]` returns exactly
one row. A module is reached through `public` (12.2) and through the sessions
that its mutations return.

### 12.9 A complete module

```liasse
//! A task list with a public API.
meta { name: "example.tasks", version: "1.0.0", language: "liasse 0.0.1" }

tasks: Table {
    id = uuid()
    unique(.id)
    title: Text { assert(size(.) > 0, "A title is required") }
    done = false
    created_at = now()
    mut complete() { .done = true }
}

public {
    view open_tasks = .tasks
        |> filter(!.done)
        |> sort(desc(.created_at), .id)
        |> map { .id, .title, .created_at }
    mut add(title: Text) {
        let task = .tasks += { title: string.trim(title) }
        return task |> map { .id, .title, .done, .created_at }
    }
    mut complete(id: Uuid) { .tasks[id].complete() }
}
```

`add` with the `title` `"  Read the specification  "` inserts a task titled
`"Read the specification"` and returns it, and `open_tasks` then lists it
first. With a blank title, the `assert` fails with the message
`A title is required`, and `add` changes nothing.

---

## 13. Migrations

### 13.1 Source folder and package

```text
tasks/                                     source folder
  tasks.liasse                             current definition: meta { name: "acme.tasks", version: "1.2.0", … }
  limits.lib.liasse                        a library file of the current definition
  children/…                               the libraries that the definitions require, each a module
  migrations/
    1.0.0/tasks.liasse                     every earlier version: its module file
    1.0.0/limits.lib.liasse                and its library files
    1.1.0/tasks.liasse
    1.1.0/limits.lib.liasse
    1.0.0-1.1.0.migration.liasse           every migration between consecutive versions
    1.1.0-1.2.0.migration.liasse

tasks.liasse.zip                           package: the same layout
```

A version is its module file with its library files, and `migrations/` holds
every earlier one. The libraries in `children/` are modules of their own
(12.4): every version resolves its requirements to them (12.5), and
elaborates from its own files and those libraries.

### 13.2 Migration files

A migration file states the structural change between two versions with
declarations, and what happens to the data in each direction with programs:

```liasse
//! 1.1.0 -> 1.2.0
meta { name: "acme.tasks", from: "1.1.0", to: "1.2.0" }

add    /tasks { notes: Text? }
add    /archive { entries: Table { id: Uuid; text: Text } }
remove /tasks { draft: Text? }
remove /archive { items: Table { id: Uuid; body: Text } }
rename /tasks/name to /tasks/title
retype /tasks/done from Text to Bool {
  up: old == "done"
  down: old ? "done" : "open"
}

up {
  drop /tasks/draft
  .archive.entries = old.archive.items |> map { .id, text: .body }
}
down {
  drop /tasks/notes
  .archive.items = old.archive.entries |> map { .id, body: .text }
}
```

The same migration, in `in /path { … }` scopes:

```liasse
meta { name: "acme.tasks", from: "1.1.0", to: "1.2.0" }

in /tasks {
  add { notes: Text? }
  remove { draft: Text? }
  rename name to title
  retype done from Text to Bool {
    up: old == "done"
    down: old ? "done" : "open"
  }
  up { drop draft }
  down { drop notes }
}

in /archive {
  add { entries: Table { id: Uuid; text: Text } }
  remove { items: Table { id: Uuid; body: Text } }
  up { .entries = old.items |> map { .id, text: .body } }
  down { .items = old.entries |> map { .id, body: .text } }
}
```

The `meta` of a migration has exactly `name`, the package name of both
versions, and `from` and `to`, two consecutive versions of the chain. A
migration file is read in the language version of its `to` definition. An
`add` or `remove` without a path applies to its scope's node.

### 13.3 Data changes

A **data change** is a stored member, collection, or object added, removed,
renamed, or retyped. Only data changes need a migration; every other change
takes effect with the update.

The **data structure** of a version is the tree of its stored members after
elaboration: for each body, the names of its stored members (fields, objects,
collections, and mount points) and, for each, its kind, its type with its
optionality, and the data structure of its body, where a type counts by its
name, whatever its arguments. Applying a migration's `add`, `remove`, `rename`,
and `retype` to the data structure of its `from` version MUST yield exactly the
data structure of its `to` version. Two consecutive versions whose data
structures differ have a migration file between them. The build names the first
path where either check fails.

### 13.4 Restated declarations

A `remove` restates the declarations it removes, a `retype` states both types,
and an `add` states the declarations it adds; the build checks each against
the declaration it names. The names in an `add` body and in a retype's `to`
type resolve as in the body that the path names in the `to` definition, and
those in a `remove` body and a retype's `from` type as in the `from`
definition. The names in the `up` programs and mappings resolve in the `to`
definition, and those of `down` in the `from` definition, as in the body of
the node their scope names, the root at the file level.

### 13.5 Paths

A migration path is a path of the definition, from its top level: `/`, then
the names of the declarations that hold one another, one name per body.
`/tasks/done` is the member `done` in the body of the collection `tasks`, and
`/archive/items` the collection `items` in the body of the object `archive`. A
path names declarations, whatever a template does with them: through a
collection, it goes to a member of its row body, and on to the members inside
that member. Inside `in /path { … }`, a relative path starts at that
declaration, and scopes nest.

- `add` names a body of the `to` version, `remove` a body of the `from`
  version, and `retype` a member of both.
- `rename` names a `from` path and a `to` path that differ in their last name
  once the file's renames of enclosing declarations apply, so a declaration
  keeps its owner.
- The flat and the nested forms mean the same, and mix.
- A path stays within one module: a child's data follows the child's own chain.

### 13.6 Programs

`up { … }` and `down { … }` are programs (chapter 9) that run after the
structural change of their direction. At the file level `.` is the new root
and `old` the complete, read-only previous root. Inside a scope they are the
scope's node: for an object, the object of each owner; for a collection, each
row that the migration carries, `.` its new value and `old` its previous value.
`old` has the type that the other version gives its node. A retype's `up` and
`down` map one value, `old` being the member's previous value, to its new
value. Programs read `.` and `old`, write, and call functions; mappings read
`old` and call functions.

### 13.7 Every loss is accounted for

Every migration works in both directions. Every value that leaves state in a
direction, a removed member going up, an added member going down, and a
retype's source values in each direction, is accounted for in that direction:

- by `drop path` in a program of that direction, naming its path or a path that
  contains it;
- by a statement of that direction that reads it through `old`, where reading a
  member accounts for that member's path and the collections and objects that
  hold it, and reading a collection or object as a whole accounts for every
  path under it;
- for a retype, by its mapping.

The children that a removed mount point held are values that leave state. The
build names every path left unaccounted for, and a `drop` names a path that
leaves state in its direction. A required member that enters state has an
initializer, or a program of that direction writes it.

---

## Annex A. Grammar

The grammar of the three kinds of source file. Chapter 2 defines the tokens and
line breaks.

```text
// Files
Module         = InnerDocs? (RootEntry % EntrySep)?                    // a `.liasse` file
RootEntry      = Docs? (Meta | Requires | Use | Declaration)
LibraryFile    = InnerDocs? ((Docs? LibraryDecl) % EntrySep)?          // a `.lib.liasse` file
LibraryDecl    = Function | Template | Mixin | Interface
MigrationFile  = InnerDocs? MigrationMeta (EntrySep MigrationEntry)* EntrySep?  // a `.migration.liasse` file

Meta           = 'meta' '{' (MetaMember % MemberSep) '}'
MetaMember     = Docs? NAME ':' TEXT
Requires       = 'requires' '{' ((NAME ':' TEXT) % MemberSep)? '}'
Use            = 'use' NAME '.' (UseItem | '{' UseItem % ',' '}' | '*')
UseItem        = NAME ('as' NAME)?

// Declarations
Body           = '{' InnerDocs? (Entry % EntrySep)? '}'
Entry          = Docs? (Declaration | SlotFill)
SlotFill       = NAME Body
Declaration    = Data | View | Mut | Function | MixinApp | Slot | Template | Mixin
               | Public | Interface | Mount
Data           = NAME ':' Definition ('=' Expression)? | NAME '=' Expression
View           = 'view' NAME Params? (':' Definition)? ('=' Expression)?   // '=' outside an interface
Mut            = 'mut' NAME Params (':' Type)? Program?                    // Program outside an interface
Function       = 'fn' NAME Params ':' Type ('=' Expression | FnBlock)
FnBlock        = '{' InnerDocs? (FnEntry EntrySep)* 'return' Expression EntrySep? '}'
FnEntry        = Docs? ('let' NAME '=' Expression | Function)
MixinApp       = QualName '(' Args ')' Body?
Slot           = 'slot' NAME? Body?
Template       = 'template' NAME Params? ':' Definition
Mixin          = 'mixin' NAME Params? Body
Public         = 'public' Body
Interface      = 'interface' NAME InterfaceBody
InterfaceBody  = '{' InnerDocs? ((Docs? (View | Mut)) % EntrySep)? '}'
Mount          = 'mount' NAME (':' QualName | InterfaceBody)

// Definitions and parameters
Definition     = QualName ('(' Args ')')? '?'? Body? '?'?              // at most one '?'
               | Body '?'?
Type           = QualName ('(' Args ')')? '?'? | Body '?'?             // a definition that ends before any body
QualName       = NAME ('.' NAME)?
Params         = '(' (Param % ',')? ')'
Param          = Docs? NAME (':' Kind)? ('=' Expression)?
Kind           = 'type' | '.' '->' Definition | Definition

// Programs
Program        = '{' (Statement % EntrySep)? '}'
Statement      = Docs? 'let' NAME '=' (Write | Expression)
               | Write
               | Expression                                            // a call made for its effect
               | 'return' (Write | Expression)
               | Drop                                                  // in a migration program
Write          = Target ('=' | '+=' | '-=') Expression | Target PatchBlock
Target         = Postfix
PatchBlock     = '{' (PatchMember % MemberSep)? '}'
PatchMember    = NAME '=' Expression | '=' Path | NAME PatchBlock

// Expressions
Expression     = Conditional ('|>' Mapping)*
Mapping        = QualName ('(' Args ')')? Record?
Conditional    = Combination ('?' Expression ':' Conditional)?
Combination    = Fallback (('|' | '&') Fallback)*
Fallback       = Or ('??' Or)*
Or             = And ('||' And)*
And            = Equality ('&&' Equality)*
Equality       = Comparison (('==' | '!=') Comparison)*
Comparison     = Additive (('<' | '<=' | '>' | '>=' | 'in') Additive)*
Additive       = Multiplicative (('+' | '-') Multiplicative)*
Multiplicative = Unary (('*' | '/' | '%') Unary)*
Unary          = ('!' | '-')* Postfix
Postfix        = Primary ('.' NAME | '.' '$' NAME | '[' Selector ']' | '(' Args ')')*
Primary        = Literal | Root | Record | Tuple | Array | '(' Expression ')'
Root           = '/' NAME? | '.' NAME? | '.' '$' NAME | '^'+ ('.' NAME)? | '#' NAME | '@' NAME | NAME
Path           = Root ('.' NAME)*
Selector       = Expression % ','
Record         = '{' (Member % MemberSep)? '}'
Member         = NAME ':' Expression | Shorthand
Shorthand      = NAME | '@' NAME | '.' NAME | '/' NAME | '^'+ '.' NAME   // a short path
               | ('#' | '@')? NAME '.' NAME
Tuple          = '(' ')' | '(' Expression ',' ')' | '(' Expression (',' Expression)+ ')'
Array          = '[' (Expression % MemberSep)? ']'
Args           = (Arg % MemberSep)?
Arg            = NAME ':' Argument | Argument
Argument       = Lambda | Expression | Definition                      // read by the parameter's kind
Lambda         = '.' '->' Expression
Literal        = INT | DECIMAL | TEXT | 'true' | 'false' | 'none'

// Migration files
MigrationMeta  = 'meta' '{' 'name' ':' TEXT MemberSep 'from' ':' TEXT MemberSep 'to' ':' TEXT '}'
MigrationEntry = Docs? ( 'in' MigrationPath '{' (MigrationEntry % EntrySep)? '}'
                       | 'add' MigrationPath? Body
                       | 'remove' MigrationPath? Body
                       | 'rename' MigrationPath 'to' MigrationPath
                       | 'retype' MigrationPath 'from' Type 'to' Type
                         '{' 'up' ':' Expression MemberSep 'down' ':' Expression '}'
                       | 'up' Program
                       | 'down' Program )
MigrationPath  = '/' | ('/' NAME)+ | NAME ('/' NAME)*                  // absolute, or relative inside `in`
Drop           = 'drop' MigrationPath % ','

// Separators and documentation
EntrySep       = NEWLINE | ';'
MemberSep      = NEWLINE | ','
Docs           = ('///' line | '/**' … '*/')+
InnerDocs      = ('//!' line | '/*!' … '*/')+
```
