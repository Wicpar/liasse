# The Liasse Language

Language version `liasse 0.0.1`.

This document defines the Liasse language: the syntax of its source text and
what each form means. [BUILTINS.md](BUILTINS.md) defines the builtins. A
source file holds one module (chapter 14), one migration (chapter 15), or one
library (chapter 16).

---

## 1. Notation

**MUST**, **MUST NOT**, and **MAY** are used as in BCP 14.

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

Every `liasse` block derives from Annex A: from `Module`, `MigrationFile`, or
`Library` when it shows a whole file, and otherwise from the entries of a
`Body`, the statements of a `Program`, or one `Expression`.

---

## Part I. Text

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
- Each operator is the longest that the characters form: `a!=b` is `a != b`,
  and in `T! = e` a space separates `!` from `=`.
- Source is UTF-8. Whitespace and comments separate tokens, and 2.4 gives the
  role of each newline.

The reserved words are `let`, `return`, `mut`, `view`, `fn`, `slot`, `template`,
`mixin`, `public`, `use`, `in`, `none`, `true`, and `false`. A name that an
author chooses differs from every reserved word. A member or argument name that
a builtin or a requirement fixes MAY be one, written after `.` or before `:`:
`.keys.public`.

These names have a meaning only where the language admits them: `set` in the
body of a definition of cardinality `*` or `+` (4.5); `meta` and `requires` at
the root of a module file (14.1, 14.3); `as` in a `use` entry (14.3); `old` in a
migration (15.5); and `add`, `remove`, `rename`, `retype`, `from`, `to`, `up`,
`down`, and `drop` in a migration file (15.1).

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
- a newline directly before a `{` that opens a body, a block, or the record
  after the arguments of a call;
- a newline directly before a line that begins with `|>`;
- a newline directly after a prefix or binary operator of an expression, `|>`
  and `->` included;
- a newline directly before or after the `?` or the `:` of a conditional.

A value starts on the line of the `=`, `+=`, or `-=` before it. The members of
a record, an argument list, a set, a block, `meta`, `requires`, and a retype
mapping are separated by a newline or `,`. A trailing separator is allowed.

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

## Part II. Data

## 3. Values

### 3.1 Literals

```liasse
retries: Int = -7
ratio: Decimal = 3.50
fee: Decimal = 3            // the decimal 3
city: Text = "Paris"
active: Bool = true
starts_on: Date = "2026-09-25"
timeout: Duration = "PT5M"
```

An integer, a decimal, a text, `true`, and `false` are literals, and `none` is
the empty set (3.3). An integer is an `Int`, a decimal a `Decimal`, a text a
`Text`, and `true` and `false` are `Bool`s. Where another host type is
expected, a literal spells a value of that type, in a form that the type reads.
Each type reads its literals when the module is built, and a literal that spells
no value of its type fails the build, as `"2026-02-30"` does for a `Date` and
`3.5` for an `Int`.

### 3.2 Records and tuples

```liasse
origin = { city: "Paris", country: "FR" }   // a record
span = { low: 10, high: low + 5 }           // a member reads a sibling
pair = ("daily", 100)                       // a tuple
single = ("daily",)                         // a one-tuple
```

A record's members match by name, in any order, and a member MAY read a sibling
member by its bare name, acyclically. `(a)` is `a` in parentheses; `()` is the
empty tuple.

### 3.3 Sets

```liasse
limits: Int* = [10, 20, 50]                 // a set of three elements
limit = 10                                  // a set of one element
```

Every value is a **set**: zero or more **elements**. A literal, a record, and a
tuple are one element. `[a, b, …]` is the set of the elements of each
expression, in that order, of the type that its position gives, and `[]` is
`none`.

---

## 4. Declarations

### 4.1 Data members

```liasse
title: Text                 // a member of type Text
retry_limit: Int = 3        // with an initializer
label = "Open"              // the initializer gives the type
```

A data member is stored state. Its initializer gives its first value, and writes
(chapter 10) then set any value of its type: `done = false` is a `Bool` that a
mutation sets to `true`. A value that a write or an insertion supplies, `none`
included, takes the place of the initializer. The initializer of a member whose
cardinality allows zero is `none` when left out. A required member of the module
root MUST have an initializer, or be built from the initializers of its members.

### 4.2 Cardinality

```liasse
title: Text                 // one
nickname: Text?             // zero or one
tags: Text*                 // zero or more
owners: Text+               // one or more
due: Date? = none
```

A type's **cardinality** is how many elements its sets hold: `T` exactly one,
`T?` zero or one, `T*` zero or more, and `T+` one or more. A member of
cardinality one is **required**, and one of cardinality `?` is **optional**.
One cardinality **fits** another when the other allows each number of elements
that it allows: one fits each cardinality, `?` and `+` fit `*`, and each fits
itself. A value that a member stores, and a value given where a type is
declared, has a cardinality that fits the declared one.

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
with its owner. Its value is a record with exactly its members.

### 4.4 Definitions

What follows `:` is a **definition**:

```liasse
title: Text
tags: Text*
contract: Blob(storage: "archive")?
people: { name: Text }*
```

A definition is a type name, optionally qualified by a namespace (`ns.Name`),
optionally applied to arguments, optionally followed by a body, and optionally
marked with a cardinality (4.2). `Text? { … }` and `Text { … }?` are the same
definition. The definition of a data member takes one cardinality mark at most,
the mark of the template that it applies included, and another definition MAY
take several marks, of cardinality or `!` (6.4). A plain `{ … }` is an object
(4.3).

A name applied to arguments is a **template**: a named definition with
parameters. `Map<Text, Int>` applies the template `Map` with its type
parameters bound to `Text` and `Int`, and `Blob(storage: "archive")` applies
`Blob` with `storage` bound to `"archive"`. A definition is evaluated when the
module is built. Chapter 13 declares templates.

Type arguments stand in `<…>`, and the other arguments in `(…)`. Each list
gives its arguments all by name, in any order, or all by position, in the
order of the parameters:

```liasse
limits: Map<Text, Int>
quotas: Map<K: Text, V: Int>        // the same definition, by name
archive: Blob(storage: "archive")
vault: Blob("archive")              // the same definition, by position
```

By position, the arguments fill the first parameters; by name, the parameters
that they name. Every other parameter takes its default, and every other type
parameter is bound by the arguments (9.3).

### 4.5 Stored sets

```liasse
tasks: {
    title: Text
    done = false
}*
labels: Text* { set { unique() } }      // no label twice
```

A data member of cardinality `*` or `+` is a **stored set**, and its elements
are **rows**: each row of `tasks` has the members that its body declares, and
each row of `labels` is a text. By default, the rows of a stored set are in the
order of their insertion, and reading them keeps that order.

A block `set { … }` in the body of a definition of cardinality `*` or `+` holds
mixin applications at its **set level**, where they apply to its elements
together: in a stored set, to the rows of one owner, as `unique()` does to the
labels. The declarations that these applications inline are mixin applications.

### 4.6 Bodies

```liasse
companies: {
    //! Each row is one company.
    name: Text
    offices: { code: Text; city: Text }*
}*
```

- Within one body, data members, views, mutations, and slots have distinct
  names; templates, mixins, and functions have distinct names among themselves,
  and a none overload (9.4) has the name of its function. A nested body MAY
  reuse a name of an outer one.
- The order of entries matters for documentation and for where a slot or a
  mixin application inlines content.

### 4.7 Names

The builtins, and what else the host provides, are declared in host libraries
(16), whose entries join the module's root. A **namespace** is a named group of
published declarations: a required module (14.3), or a group of declarations of
a host library, such as `string` (16).

An unqualified name in a definition, an application, or a call denotes the
nearest enclosing declaration of that name, else what the module imports (14.3).
A qualified name `ns.name` denotes what the namespace `ns` publishes.

---

## Part III. Expressions

## 5. Expressions

### 5.1 Levels and paths

Every body is a **level**. At each level, `.` is the current value: the module
root at the root, each row in the body of a stored set, the rows of one owner at
its set level (4.5), the object in an object's body, and each element of the
member's value in a member's body. `^` is the current value one level out, `^^`
two levels out, and each further `^` one level more, as far as the root: in a
body n levels below the root, `^` stacks at most n times, and n times it is the
root. A set level stands one level below its owner, as the body of the rows
does. The `^`s of a path stand at its start.

```liasse
region = "EU"
companies: {
    name: Text
    country: Text
    offices: {
        city: Text
        name = .city                      // `.` is the office
        country = ^.country               // `^` is its company
        region = ^^.region                // `^^` is the root
        set { unique(.city) }             // `.` is the offices of one company
    }*
}*
```

| Path | Denotes |
| --- | --- |
| `.` | the current value |
| `.name` | the member `name` of the current value |
| `^`, `^^`, `^^^`, `^.name` | the current value one, two, and three levels out, up to the root, and a member of it |
| `/name` | the member `name` of the module root |
| `name` | a lexical binding: a sibling record member, a local, a parameter |

A path that reads one member `x` from where it starts, such as `.x`, `^.x`,
`^^.x`, `/x`, or `task.x`, is a **short path** named `x`, and so is a name `x`
alone. In a record, a short path is a member of its name:

```liasse
{ .title, ^.name, /region, task.id, due }
{ title: .title, name: ^.name, region: /region, id: task.id, due: due }   // the same record
```

The arguments of a definition are written at the level of its body. Levels are
fixed by the source when the module is built.

### 5.2 Calls

```liasse
offices: {
    code: Text { assert(size(.) <= 8, "Code too long") }   // `.` is the code
    set { unique(.code) }                                  // `.` is the offices
}*
```

```liasse
round(.amount, 2)                       // a builtin function
round(x: .amount, digits: 2)            // the same call, by name
string.lower(string.trim(.title))       // functions of the namespace `string`
uuid()                                  // a builtin mutation
```

`name(arguments)` is a **call**, and its position gives its meaning:

| Position | The call |
| --- | --- |
| an entry of a body | applies a **mixin**: it inlines the mixin's declarations there, as if they were written there |
| an expression | calls a function or a mutation and yields its result |
| a statement of a program (10.2) | calls a mutation |

A **function** is declared with `fn` (chapters 9 and 16), and a call of it
yields its result. A **mutation** is declared with `mut` (chapters 10 and 16),
and the changes that a call of it makes take effect at the end of its
transaction. Chapter 12 declares mixins.

Every call takes its arguments as a definition does (4.4): `f(title)` passes the
value of `title` to the first parameter. Outside a pipeline (6.3), the
parentheses are always written.

After `:`, `Name<…>(…)` applies a template (4.4). `path.name(arguments)` reads
the view `name` of the value at `path` with those arguments (8.2), or, in a
mutation, calls its mutation `name` (10.2).

### 5.3 Operators

Tightest first:

| Operators | |
| --- | --- |
| `.name` `.$name` `[…]` `(…)` `!` | postfix |
| `!` `-` | prefix |
| `*` `/` `%` | |
| `+` `-` | |
| `<` `<=` `>` `>=` `in` | comparison |
| `==` `!=` | equality |
| `&&` | |
| `\|\|` | |
| `??` | fallback |
| `? :` | conditional |
| `\|>` | pipeline (6.3) |

Binary operators of one level associate left; `? :` associates right.

```liasse
.shipped_at != none && .shipped_at > .promised_at
.nickname ?? .name
.done ? "closed" : "open"
(.balance ?? 0.0) + 1.0
```

`a ?? b` is `b` when `a` is `none`, and `a` otherwise, and `a` and `b` have one
element type. When the cardinality of `a` allows zero, `a ?? b` has the smallest
cardinality that both that of `b` and that of `a` with zero removed fit:
`.nickname ?? .name` is a `Text`, and `.tags ?? "untagged"` a `Text+`. Otherwise
it has the cardinality of `a`. `&&`, `||`, `? :`, and `??` evaluate only the
operands they select, so `b != 0 && a / b > 1` never divides by zero. The two
branches of `? :` have one element type, and the conditional has the smallest
cardinality that both branches fit.

Every other prefix or binary operator applies the **operator declaration** for
the types of its operands, and has the result type that it gives: `fn A + B: C`
declares `a + b` of an `A` and a `B` as a `C`, and `fn -A: C` declares `-a` of
an `A`. Every operator declaration is a builtin, and `==` compares by value
(5.4).

### 5.4 Equality

```liasse
task == other               // equal values
"urgent" in .labels         // set membership
.status in ["open", "held"]
```

Values compare by value: two values are equal when they have the same structure
and equal data. Records and objects are equal when they have the same members,
each equal; tuples when they hold equal values in the same order; sets when
their elements pair one to one, each equal to its pair; rows when their values,
the records of their data members, are equal.

---

## 6. Sets

### 6.1 Reading through sets

```liasse
usage: {
    amount: Decimal
    funding: { pool: Uuid; quantity: Decimal }*
}*
```

A member read through a set maps over its elements: `.usage.amount` is the set
of the amounts, and `.usage.funding` the set of every funding row of every usage
row. A path's cardinality allows zero when a step's cardinality does, and
several when a step's does: `.usage.amount` has the cardinality `*`, and
`.billing_address.city` (4.3) the cardinality `?`.

### 6.2 Selection

```liasse
.tasks[.done]                                   // the tasks whose `done` holds
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
.orders[.lines[.sku == sku] != none]            // the orders that have such a line
.people[.email == none]                         // the people with no email
```

`s[c₁, c₂, …]` is the relational selection σ: the elements of `s` for which
every condition, a `Bool`, holds. `,` joins conditions and binds loosest, so
`[a, b || c]` is `a && (b || c)`. A selection applies to any set: a member, a
view, a local, the result of a pipeline. It has the cardinality of `s` with zero
added: `?` over a value of cardinality one, and `*` over a set of cardinality
`+`.

The brackets open a level: `.` is the element being tested, and `^` the value
outside the brackets. Lexical names keep their meaning. A condition that is
exactly a short path named `x` (5.1) and starts at a name, `^`, or `/` is the
equality of `.x` with it: `.x == x`, `.x == task.x`, `.x == ^.x`, `.x == /x`.

### 6.3 Pipelines

```liasse
.tasks
    |> filter(!.done)
    |> sort((.title, .id))
    |> { .id, .title }

.invoices.total |> sum                          // `sum(.invoices.total)`
```

`x |> f(a, …)` is the call `f(x, a, …)`: the value on the left is the argument
of the first parameter, and the parentheses give the others; `x |> f` is the
call `f(x)`. What follows `|>` is a **stage**. A stage that starts with a name
is such a call, and any other stage is a function of one element: `x |> . -> e`
is the set of the values of `e`, one for each element of `x`, in their order. A
chain of stages is a **pipeline**: each stage reads the set that the one before
it yields.

A function of one element, as a stage or as the argument of a parameter of kind
`V -> T` (9.2), such as the condition of `filter`, is written `. -> e`: `.` is
the element, and `^` the current value where the function is written. `e` alone
means `. -> e`, and lexical names keep their meaning. In an expression, braces
always hold a record, and a record after the arguments of a call is the argument
of its last parameter: `.tasks |> distinct { .owner, .done }` keeps one task for
each combination of `owner` and `done`. These three are one stage:

```liasse
.tasks |> { .id, .title }
.tasks |> { id: .id, title: .title }
.tasks |> . -> { .id, .title }
```

Every other argument is evaluated at the level of the call: in
`.tasks |> difference(.archived)`, `.archived` is a member of the current value
where the call is written.

The order of the stages defines the result:

```liasse
.orders                                         // per customer, the number of orders above 100
    |> filter(.total > 100.0)
    |> group(.customer) { customer: .key, orders: count(.items) }
```

```liasse
.orders                                         // the customers whose orders total above 100
    |> group(.customer) { customer: .key, total: sum(.items.total) }
    |> filter(.total > 100.0)
```

A stage ends at the next `|>`, so a pipeline inside a stage is written in
parentheses:

```liasse
.usage                                          // the most funding rows above 10 of one usage row
    |> (.funding |> filter(.quantity > 10.0) |> count)
    |> max
```

### 6.4 Sets of sets

```liasse
fn labels(given: Text*?): Text* = given == none ? ["default"] : given!

.usage |> .funding                              // a set of sets of funding rows
(.usage |> .funding)!                           // `.usage.funding`
```

A cardinality mark on a type of cardinality one gives the type that cardinality,
and a mark on a type of another cardinality makes a **set of sets**, whose
elements are sets of that type: a `Text*?` is `none` or one set of texts, which
MAY be empty, so `labels` keeps an empty set that it is given and takes the
default for `none`.

`x!` **flattens** `x`: it is the set of the elements of each set of `x`, and
over a set whose element type has cardinality one, it is `x`. The cardinality of
`x!` allows zero when that of `x` or of its sets does, and several when either
does, as a path's does (6.1): `given!` is a `Text*`. `T!` is the type of `x!`
for an `x` of type `T`, and a path reads through a set of sets as through the
set that `!` gives: `x.name` is `x!.name`.

`S |> U` is the type of `x |> . -> e` for an `x` of type `S` and an `e` of type
`U`: `U` when `S` has cardinality one, and else `U` marked with the cardinality
of `S`. `Text+ |> Int` is `Int+`, and `.usage |> .funding` is a set of sets.

### 6.5 Metaproperties

```liasse
.$history.updated_at
task.$history.created_at
```

`x.$name` reads the metaproperty `name` of `x`, and `.$name` that of the current
value. A metaproperty is read-only, and it stands beside the members: a value
and a record built from it hold the members, and a record holds a metaproperty
by naming it, as in `{ .title, changed_at: .$history.updated_at }`. It is read
through a set as a member is (6.1).

---

## Part IV. Behavior

## 7. Initializers

An initializer is evaluated once, when its member comes into existence. In an
insertion, `.` is the row being inserted, with every supplied value and every
initializer already resolved:

```liasse
currency = "EUR"
invoices: {
    amount: Decimal
    currency = ^.currency       // the root's currency at insertion
    balance = .amount           // the amount, until payments reduce it
}*
```

A later change of the root's `currency` leaves the `currency` of every existing
invoice as it is.

An initializer reads `.`, `^` and the levels further out, `/`, and the lexical
bindings where it is written, and it calls functions and mutations, such as
`uuid()`. Initializers are evaluated in the order of what they read, which MUST
form an acyclic graph.

---

## 8. Views

### 8.1 Views

```liasse
view label = .done ? "Complete" : "Open"
view label_text: Text = .done ? "Complete" : "Open"
view open_tasks = .tasks
    |> filter(!.done)
    |> sort((.title, .id))
    |> { .id, .title }
```

A **view** defines a query. It is read like a data member, `.label`, and
declared where a data member is; declared in the body of a stored set, it is a
member of each row. Reading a view evaluates its expression where the view is
declared, over the current state. A view's type is the type of its expression,
and an annotation such as `: Text` states it. Its expression calls functions,
and views read each other along an acyclic graph. A record and a row's value
hold data members only.

```liasse
view labels = { open: "Open", complete: "Complete" }
view limit = 100
```

A view whose expression reads only literals and other constants is a
**constant**. Its value is fixed by the definition and read-only through every
path. A constant stands wherever a build-time value is required, such as a
template argument.

### 8.2 Parameters

```liasse
view by_owner(owner: Text) = .tasks[.owner == owner]
view by_status() = .tasks[.status == @status]
view task() = .tasks[@id]                   // `.id == @id`
view page(limit: Int = 20, after: Int = 0) = .tasks[.position > after, .position <= after + limit]
```

A view with parameters is read with arguments: `.by_owner("ana")`, or
`.by_owner(owner: "ana")`. A parameter is declared in the header, or by reading
`@name` in the expression, or both; `@name` and `name` are the same binding when
the header declares `name`. A parameter declared by use takes its type from the
typed positions it stands in, and every one of them gives the same type:

| Position | Gives |
| --- | --- |
| a selection condition: `[.at < @until]`, `[@id]` | the type of the element's side |
| a comparison or arithmetic with a typed operand: `.owner == @owner` | that operand's type |
| an argument for a parameter of one type: `round(.amount, @digits)`, `.page(limit: @limit)` | that type |

A parameter that stands only where several types are accepted, as an argument
for a parameter of several types or a record member such as `{ x: @x }`, is
declared in the header. A default is an expression over constants and the other
parameters. The parameters, their types, and their defaults are part of the
view's contract.

`@name` is read in a view with parameters and in the selections and pipelines
inside it.

---

## 9. Functions

### 9.1 Declaring functions

```liasse
fn slug(title: Text): Text = string.lower(string.trim(title))

fn label(title: Text, fallback: Text = "Untitled"): Text {
    let t = string.trim(title)
    fn blank(s: Text): Bool = size(s) == 0
    return blank(t) ? fallback : t
}

tasks: { title: Text; view heading = label(.title) }*
```

`fn name(parameters): T = e` yields the value of `e`. A block holds `let`
bindings and local functions, in order, and ends with one `return`. The result
type is written, so a brace after it opens the block. A function is declared at
the module root, directly in the body of a template or a mixin, or in a function
block, where its scope is the rest of the block.

A body reads its parameters, its locals, the functions and namespaces in scope,
`/` in a module, and the lexical bindings where the function is declared. The
caller's `.`, `^`, and `@name` reach it as arguments, as `.title` does in
`heading`. A body calls functions, and functions call each other along an
acyclic graph, local functions and defaults included. A computation that calls a
mutation is itself a mutation, declared with `mut` (chapter 10).

### 9.2 Parameters

```liasse
fn clamp(x: Decimal, low: Decimal = 0.0, high: Decimal = 1.0): Decimal = x < low ? low : (x > high ? high : x)
fn open_count(tasks: { done: Bool }*): Int = count(tasks[!.done])
fn count_where(tasks: { done: Bool }*, keep: { done: Bool } -> Bool): Int = count(tasks[keep(.)])
view open = count_where(.tasks, !.done)        // `keep` is `. -> !.done`
```

A parameter's kind is a type `T`, for an expression of type `T`, or `V -> T`,
for a function of one value of type `V`, of result type `T`. An argument for
`V -> T` is written as in a pipeline (6.3), `. -> e` or `e` alone, and the body
applies it to a value: `keep(.)`. The parameter `tasks` takes any set whose
elements have a `done` of type `Bool` (11.1). Only trailing parameters have
defaults, evaluated for each call that leaves them out.

### 9.3 Type parameters

```liasse
fn either<V>(first: Bool, a: V, b: V): V = first ? a : b
fn unpaid<V: { paid: Bool }>(items: V*): items?! = items[!.paid]
fn double<A + A: A>(x: A): A = x + x
fn trimmed(texts: Text*): texts = texts |> . -> string.trim(.)

view shown = either(.urgent, .nickname, .title)   // `V` is `Text?`
view picked = either<Text>(true, "a", "b")        // `V` given
view due = unpaid(.invoices)                      // the unpaid invoices
view twice = double(.amount)                      // `A` is `Decimal`
view clean_title = trimmed(.title)                // a `Text`
view clean_tags = trimmed(.tags)                  // a `Text*`
```

`<…>` after the name of a declaration declares its **type parameters**, such as
`V`. A type parameter stands for a type, of any cardinality, and `V: T`
**bounds** it to the types that fit `T`, or, for a shape `T`, to the types whose
values conform to it (11.2). An **operator entry**, such as `A + A: A`, bounds
its type parameters to the types for which that operator is declared (5.3), and
in the declaration, `+` of two `A`s gives an `A`. Each name of an operator entry
is a type in scope, or else a type parameter that the entry declares.

A call binds each type parameter to the smallest type that the arguments in its
positions fit: in `shown`, `V` is `Text?`, which `.nickname` and `.title` both
fit, and in `due`, `V` is the type of the rows of `invoices`, which conform to
`{ paid: Bool }`. Type arguments MAY be given in `<…>` after the name, all by
position or all by name (4.4). In an expression, `<` after a name opens type
arguments when the `>` that closes them is directly followed by `(`.

In the types of its parameters and its result, a declaration MAY name one of its
parameters in place of a type. The name is then a type parameter bounded by that
parameter's type, which a call binds to the type of the argument: `trimmed`
yields a `Text` for `.title` and a `Text*` for `.tags`, and `unpaid` the type of
its argument with zero added (6.4).

Functions, mutations, views, templates, and mixins declare type parameters
alike.

### 9.4 None overloads

```liasse
fn spread(values: Decimal+): Decimal = max(values) - min(values)
fn spread(none): none
fn share(part: Decimal, whole: Decimal): Decimal = part / whole
fn share(part: none): none

view range = spread(.readings.value)                // a `Decimal?`
view paid = share(sum(.payments.amount), .total)    // a `Decimal?`
```

A function MAY have one **none overload**: a declaration of its name whose
parameters are `none` and whose result is `none`. It names parameters of the
function all by position or all by name, as a call gives its arguments (4.4):
`spread(none)` names `values`, and `share(part: none)` names `part`. A call
whose argument for a parameter that the none overload names is `none` yields
`none`, and the function's declaration gives the result of every other call.

A parameter that a none overload names has a type of cardinality one or `+`, and
an argument for it has a cardinality that fits the parameter's with zero added
(6.2). A call has the function's result type, with zero added when such an
argument allows zero: `spread(.items.value)` over the elements of a group (6.3),
one or more, is a `Decimal`.

---

## 10. Mutations

### 10.1 Mutations

```liasse
tasks: {
    title: Text
    done = false
    mut rename(title: Text) { .title = title }
    mut complete() { .done = true }
}*
```

A mutation declared with `mut` is a program: statements that run in order and
change state as one **transaction**, which takes effect at its end, whole, or
leaves state as it was. It is declared where a data member is, and `.` in it is
the current value of its body (5.1). A program reads state, calls functions and
mutations, and writes.

### 10.2 Statements

| Statement | Meaning |
| --- | --- |
| `let x = e` | binds `x` from the next statement to the end of the program, once, under a name distinct from every input |
| `path = e` | the place at `path` takes the value of `e`: a member or an object, on each element that the path reads through a set; a stored set, whose rows become the elements of `e` |
| `rows[conditions] = r` | **put**: every row that the equalities match becomes `r`, or `r` is inserted when none matches |
| `path += e`, `path -= e` | on a stored set, inserts and removes rows; on another place, `path = path + e` and `path = path - e`, by the operators of the place's type |
| `target { patch }` | assigns several members of the selected rows or object |
| `path.f(args)` | calls a mutation |
| `f(args)`, `ns.f(args)` | calls a mutation (5.2), or runs `assert` |
| `return e`, `return view e` | the result, a snapshot of `e` or a view of it (10.4); the last statement |

```liasse
mut add_task(title: Text) {
    let task = .tasks += { title }
    return task |> { .id, .title, .done }
}
```

Each statement runs over the state that the statements before it leave. A write
stands as a statement, or as the whole expression of a `let` or a `return`.
`rows += e` yields the rows it inserts, `rows = e` the rows of `e`, a put the
rows it writes, `rows -= e` the rows it removes as they stood before, and a
mutation call its result. A program holds at least one statement. The result
type is the annotation, `mut f(…): T` for a snapshot and `mut f(…): view T` for
a view, or the type of the `return` expression.

**Stored sets.** `rows += e` inserts a new row for each element of `e`, and
`rows -= e` removes the rows equal to an element of `e` (5.4). The rows nested
in a removed row go with it.

```liasse
mut purge_archived() { .tasks -= .tasks[.archived] }
mut untag(id: Uuid, tag: Text) { .tasks[id].tags -= tag }
```

**Put.** `rows[conditions] = r` inserts or replaces. Its conditions are
equalities `.m == v`, or their shorthands, on distinct stored members, each `v`
of its member's type, or `. == v` on rows that are scalars, `v` of the row's
type; `r` is the value of one row, and repeats a condition member only with an
equal value.

```liasse
mut tag_once(id: Uuid, tag: Text) { .tasks[id].tags[. == tag] = tag }       // adds `tag` when it is missing
mut set_limit(key: Text, value: Int) { .limits[.key == key] = { value } }    // sets the entry
```

**Patch.** A block after a target assigns several members, each right-hand side
reading the state at the start of the patch. `=path` assigns the member named by
the path's last name, and a nested block patches an object member.

```liasse
mut update(id: Uuid, name: Text, locale: Text) {
    .people[id] { =name, email = none, profile { =locale } }
}
```

**Targets.** A write reaches stored rows through a path, a selection, or a
pipeline that keeps rows, and it writes each row that it reaches, zero or more.
One statement writes each row once. A required member takes a value of
cardinality one:

```liasse
mut archive_done() { (.tasks |> filter(.done)).archived = true }
mut adopt_nickname() { .title = .nickname ?? .title }
```

**Mutation calls.** `path.f(args)` runs the mutation `f` that the body of the
path's value declares, once for each element of that value, with `.` bound to
it: each selected row, an object, or `/` for the root. A view that yields rows
or an object reaches their mutations too: `.open_tasks[id].complete()` completes
a task that the view `open_tasks` yields. A mutation call runs inside its
caller's transaction, and mutations call each other along an acyclic graph. A
block after a mutation call patches the rows or the object that the call returns
as a view.

### 10.3 Inputs

```liasse
mut rename(title: Text) { .title = title }     // declared
mut rename() { .title = @title }               // declared by use
mut rename(title) { .title = title }           // declared, type from the body
```

All three declare one required `Text` input named `title`. Inputs are values:
each argument is evaluated once, where the mutation is called. They are declared
as a view's parameters are (8.2), and a program adds typed positions:

| Position | Gives |
| --- | --- |
| an assignment or a patch: `.title = @title`, `{ =@title }` | the member's type |
| a member of an inserted or put record: `.data += { @value }` | the row member's type |
| an object assignment: `.address = @address` | the object's type |
| `let x = @value` | the positions of `x` |

`return @x` and the whole operand of `+=` or `-=`, as in `.tags += @tag`, accept
several types.

```liasse
mut schedule(title: Text, at: Timestamp = now(), note: Text? = none) {
    .events += { title, at, note }
}
```

- A default reads the other inputs by their bare names and constants, and calls
  functions and mutations. Defaults that read each other form an acyclic graph.
- An input whose cardinality allows zero, or with a default, is omissible:
  omission gives the default, else `none`. Every other input is required. An
  inferred input that needs a default is declared in the header.
- `@name` is read in a program and in the selections and pipelines inside it. A
  mutation that it calls receives only the arguments that its call passes.
- `{ @value }` is `{ value: @value }`, and `{ =@value }` is `value = @value`.

An input's type is a named type such as `Text` or `Timestamp`, an object, or a
template application whose definition is one of these, each of any cardinality.
A body on the type judges each element bound to the input, and its set level
(4.5) judges those elements together:

```liasse
mut retitle(title: Text { assert(size(.) > 0, "A title is required") }) { .title = title }
mut tag(tags: Text* { set { unique() } }) { .tags += tags }
```

The inputs are bound, and their checks judged, before the first statement runs.
The **contract** of a mutation is its inputs' names, types, and checks, and its
result: a snapshot or a view, and its type.

### 10.4 Snapshots and views

```liasse
tasks: {
    title: Text
    done = false
    mut complete() { .done = true }
}*

mut add(title: Text) {
    let task = .tasks += { title }
    return task |> { .title, .done }
}

mut open(title: Text): view { title: Text; done: Bool; mut complete() } {
    let task = .tasks += { title }
    return view task
}

mut draft() {
    let task = .open("Draft")
    task.complete()                  // `task.done` now reads `true`
}
```

`return e` returns a **snapshot**: the value of `e` in the state that the
program leaves. `add` returns the new task's `title` and `done`, `false`,
whatever later happens to the task.

`return view e` returns a view of `e`, read as a declared view is (8.1): each
read of it evaluates `e` over the state at that read, during the transaction and
after it. Through it, the rows and objects that it yields present what the
declared type of `mut f(…): view T` lists, and without a declared type, their
data members and views. A mutation of theirs is called through the view when `T`
lists it: `draft` completes the task that `open` returns through the `complete`
that `open` lists.

The inputs and the `let` bindings that `e` reads keep the values that they hold
at the `return`. A binding of rows keeps those rows and reads their members at
each read, and a row leaves the view when it is removed. A write or a mutation
call after `return view` runs once, and the view is of what it yields; the rest
of `e` calls functions, as a view does.

---

## Part V. Composition

## 11. Shapes

### 11.1 Shapes

```liasse
profile: {
    name: Text = ""
    email: Text = ""
    mut rename(name: Text) { .name = name }
}

mut me(): view { name: Text; mut rename(name: Text) } {
    return view .profile
}
```

In the type of a parameter or of a result, and in a bound (9.3), a definition is
a **shape**: it lists what a value has. `{ … }` is the shape of one value, and
with a cardinality the shape of the elements of a set, as the parameter `tasks`
of `open_count` has the shape `{ done: Bool }*` (9.2). A shape lists members
`name: T`, each a data member or a view; views with parameters,
`view name(parameters): T`; and mutations, `mut name(parameters)`, with an
optional result, `: T` or `: view T` (10.4). Its views and mutations are
signatures, and `view name: T` is `name: T`.

### 11.2 Conformance

A value **conforms** to a shape through its members. It has, for each `name: T`,
a data member or a view `name` whose type conforms to `T`, a set conforming when
its cardinality fits that of `T` and each of its elements conforms to the
element shape; for each view with parameters, a view of that name with the same
parameters and a conforming type; and for each mutation, one of that name with
exactly its inputs, of the same names and types, and a result that conforms.

Through a shape, a value presents exactly the members that the shape lists: the
view that `me` returns reads `name` and calls `rename`, and `email` stays inside
`profile`. A shape that lists a mutation or a view with parameters types a view
result, `: view S`.

---

## 12. Mixins

### 12.1 Declaring mixins

```liasse
mixin archivable {
    archived = false
    mut archive() { .archived = true }
}

tasks: {
    archivable()
    title: Text
}*
```

A mixin is a named fragment of declarations. An application inlines them at its
position, with the arguments substituted and its own applications inlined in
turn, so `tasks` has the members `archived` and `archive`. Each application
declares its own members. A mixin is declared at the module root, in an object
or row body, or in a template or mixin body.

### 12.2 Parameters

```liasse
mixin labelled(field: Text, message: Text = "A label is required") {
    view label = string.trim(field)
    assert(size(field) > 0, message)
}

documents: {
    title: Text
    labelled(field: .title)
}*
```

A mixin takes its arguments as a definition does (4.4): `labelled(.title)` is
`labelled(field: .title)`. It declares type parameters as a function does (9.3).
A parameter's kind is one of:

| Kind | Argument |
| --- | --- |
| a type `T` | an expression of type `T` |
| `V -> T` | a function of one value of type `V`, of result type `T` (9.2) |
| left out | an expression of any type |

An argument is written at the level of the body that applies the mixin, and
evaluated where the mixin reads its parameter, with the levels and names of the
place where it is written: in `documents`, `field` is the title of each row.
Where a use needs a build-time value, such as the argument of a definition, the
argument is a constant or an expression over constants. A default is an
expression over the other parameters and constants, and the defaults are
acyclic.

### 12.3 The current value in a mixin

A mixin's declarations take the level of the body that applies it (5.1). Through
`.`, they read the members that its expansion declares; a mixin reads a member
of its host through an argument, as `labelled` reads `.title`. The names in a
mixin's declarations resolve where the mixin is declared.

A name that a mixin's expansion and its host both declare, or that two
applications in one body both declare, is one name declared twice. Mixins apply
each other along an acyclic graph. An application stands where every declaration
of its expansion stands.

```liasse
mixin not_blank(message: Text = "Required") {
    assert(size(string.trim(.)) > 0, message)
}

people: {
    name: Text { not_blank() }
    city: Text { not_blank(message: "A city is required") }
}*
```

---

## 13. Templates

### 13.1 Declaring templates

```liasse
template Task: {
    title: Text
    done = false
    mut complete() { .done = true }
}

tasks: Task*
archived: Task*
```

Applying a template (4.4) yields its definition with the arguments substituted
and the slots filled, and that definition is elaborated like any other,
templates it applies included. `tasks` and `archived` are two independent stored
sets: each application owns its own state, and its members sit at the
application's path. A template is declared where a mixin is, and its
definition's body is an object or row body. In the type of a parameter or of a
result, a template names a shape (11.1).

```liasse
template Company: {
    name: Text
    subcompanies: Company*
}
```

A template refers to itself by name. Its values are finite: an empty set ends
the recursion, and its applications form a finite graph.

### 13.2 Parameters

```liasse
template Entries<Value>(title_limit: Int = 200): {
    title: Text { assert(size(.) <= title_limit, "Title too long") }
    payload: Value
}*

notes: Entries<Value: Text>(title_limit: 120)
counts: Entries<Int>                    // `Value` by position
```

```liasse
template Keyed(by): { set { unique(by) }; slot }*

offices: Keyed(by: (.country, .code)) {
    country: Text
    code: Text
}
```

A template's type parameters are declared as a function's (9.3), and its other
parameters have the kinds of a mixin's (12.2). Its arguments are written at the
level of the definition's body (5.1): `(.country, .code)` reads the members of
each row of `offices`. A parameter is a lexical binding inside the template's
definition, and every name in the definition resolves where the template is
declared.

### 13.3 Slots

A slot marks where a template inlines the content that its application supplies.

```liasse
template Task: {
    title: Text
    done = false
    slot
}

tasks: Task* {
    assignee: Text?
    assert(!.done || .assignee != none, "A completed task names its assignee")
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
template Ledger: {
    entries: {
        amount: Decimal
        slot entry
    }*
    slot
}*

payments: Ledger {
    label: Text
    entry { assert(.amount <= 1000.0, "One payment cannot exceed 1000") }
}
```

- `slot` is the default slot, and `slot name` a named slot, whose name differs
  from `set` (4.5). A definition has at most one default slot and one slot of
  each name. The nearest enclosing template or mixin owns a slot, at any depth.
- The bare entries of an application's body, its blocks `set { … }` included,
  fill the default slot, and a block `name { … }` fills the slot `name`
  wherever it is declared.
- A slot with a body has fallback content, used when the application leaves the
  slot unfilled. A present empty block discards the fallback. Supplied content
  takes the fallback's place as a whole.
- Slot content resolves its names where the application is written. It takes the
  level where the slot lands (5.1), and through `.` it reads every member of
  that value, the template's members included.
- Slot content adds declarations: their names differ from the template's in the
  body where they land, the template's checks hold, and the template's
  parameters stay inside its definition.

```liasse
mixin priced(currency: Text) {
    amount: Decimal
    currency = currency
    slot
}

orders: {
    priced(currency: "EUR") { tax: Decimal }
}*
```

A mixin declares slots as a template does, and the body of its application fills
them.

### 13.4 Offered templates and mixins

```liasse
template Board: {
    name: Text
    template Card: {
        title: Text
        view board = ^.name                 // `^` is the board row
        slot
    }*
    mixin pinnable { pinned = false; mut pin() { .pinned = true } }
    slot
}*

boards: Board {
    cards: Card { pinnable() }
    archive: Card
}
```

A template or a mixin declared directly in a template's body is offered to the
content of every slot of that template and to the template's own declarations;
one declared in a slot's body is offered to that slot's content and fallback. An
offered template or mixin is applied by its bare name, and it joins the names
that slot content resolves: a clash with a name visible where the application is
written is one name declared twice.

An offered template or mixin resolves its names where it is declared, the
template's parameters included, and it reaches the levels of the template that
declares it through `^`, as a declaration at the root reaches the root through
`/`: in `Card`, `^` is the board row.

What each slot binds, and the templates and mixins that it offers, are part of
the template's contract.

---

## Part VI. Modules

## 14. Modules

### 14.1 Module files

```liasse
//! A task list.
meta { name: "example.tasks", version: "1.0.0", language: "liasse 0.0.1" }

tasks: {
    title: Text
    done = false
}*
```

A module file holds one module, whose root is a body. `meta` opens it and has
exactly the members `name`, `version`, and `language`, each a text: the name
and the version of the module, and the version of the language that the file
is written in, `"liasse <version>"`.

### 14.2 Public members

```liasse
public {
    view open_tasks = .tasks[!.done]
    mut add(title: Text) { .tasks += { title } }
}
```

`public { … }` holds what a module publishes. It stands at most once, at the
root of the module file. It holds views, mutations, functions, templates, and
mixins, and mixin applications that expand to them. Its views and mutations are
members of the root, `.open_tasks`, and `.` in it is the root. Across the
module's boundary, its views present what they yield as a returned view presents
it to its caller (10.4).

### 14.3 Requirements and imports

```liasse
//! Text helpers.
meta { name: "acme.text", version: "1.2.0", language: "liasse 0.0.1" }

fn blank(t: Text): Bool = size(string.trim(t)) == 0

public {
    fn slugify(t: Text): Text = string.lower(string.trim(t))
    template Slug: Text { assert(!blank(.), "A slug is not empty") }
    mixin not_blank(message: Text = "Required") { assert(!blank(.), message) }
}
```

```liasse
meta { name: "acme.tasks", version: "1.0.0", language: "liasse 0.0.1" }
requires { text: "acme.text@1" }
use text.slugify
use text.Slug as TaskSlug

tasks: {
    title: Text { text.not_blank() }
    slug: TaskSlug = slugify(.title)
}*
```

A `requires` entry binds a local name to the namespace of a module, which its
text names by a name and a major version. The namespace of a module holds the
functions, templates, and mixins that its `public` holds, as `acme.text` does.
The local name differs from the root members. What a namespace publishes is
named through its local name, `text.not_blank`, or brought unqualified by
`use`. A published definition resolves its names in the module that declares
it, as `Slug` reads `blank`, and reaches the state of another module through
its parameters alone.

```liasse
use text.slugify
use text.slugify as slug
use text.{ slugify, Slug as TaskSlug }
use string.*
```

`use ns.name` brings one name that a namespace publishes, and
`use ns.name as local` brings it under the name `local`. `use ns.{ … }` brings
the listed names, each with its own `as`, and `use ns.*` every name that the
namespace publishes. `use` entries stand at the root of a module file, after
its `requires`, and act in the whole module. A name that `use` brings denotes
what its qualified form denotes.

In the resolution of an unqualified name (4.7), the root counts among its
declarations the names of its explicit `use` entries, and what the module
imports is what its `use ns.*` entries bring. The `ns` of `ns.name` and of a
`use` entry resolves to a requirement, and else to a namespace of a host
library. A name that an explicit `use` brings is distinct from the root's
declarations and the other explicit `use` names; a name that two `use ns.*`
bring from different definitions is written qualified.

### 14.4 Provided context

```liasse
//! The personal data of one user.
meta { name: "acme.personal", version: "1.0.0", language: "liasse 0.0.1" }

user: Parent {
    view id: Uuid
    view region: Text
    mut notify(message: Text)
}

email: Text = ""

public {
    view home = .user.region
    mut set_email(email: Text) {
        .email = email
        .user.notify("The email changed")
    }
}
```

```liasse
users: {
    id = uuid()
    set { unique(.id) }
    region: Text
    notices: { message: Text }*
    data: Module {
        view home: Text
        mut set_email(email: Text)
        provide {
            view id = ^.id
            view region = ^.region
            mut notify(message: Text) { ^.notices += { message } }
        }
    }?
}*
```

A member of the template `Parent`, here `user`, declares the context that the
module needs from its parent: its body is a shape (11.1) of the views and the
mutations that the parent provides, which the module reads and calls through the
member. `.user.region` evaluates the view `region` that the parent provides,
here in the `provide` block of the mount point `data`, where `^` is the user
row, and `.user.notify(…)` runs the `notify` that the parent provides, which
adds a notice to the user row in the transaction of `set_email`.

The rest of the body of `data` is a shape (11.1): the views and the mutations
that the parent reads and calls in the mounted module, `home` and `set_email`,
which the module's `public` alone satisfies (14.2).

---

## 15. Migrations

### 15.1 Migration files

A migration file states the structural change between two versions with
declarations, and what happens to the data in each direction with programs:

```liasse
//! 1.1.0 -> 1.2.0
meta { name: "acme.tasks", from: "1.1.0", to: "1.2.0" }

add    /tasks { notes: Text? }
add    /archive { entries: { id: Uuid; text: Text }* }
remove /tasks { draft: Text? }
remove /archive { items: { id: Uuid; body: Text }* }
rename /tasks/name to /tasks/title
retype /tasks/done from Text to Bool {
  up: old == "done"
  down: old ? "done" : "open"
}

up {
  drop /tasks/draft
  .archive.entries = old.archive.items |> { .id, text: .body }
}
down {
  drop /tasks/notes
  .archive.items = old.archive.entries |> { .id, body: .text }
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
  add { entries: { id: Uuid; text: Text }* }
  remove { items: { id: Uuid; body: Text }* }
  up { .entries = old.items |> { .id, text: .body } }
  down { .items = old.entries |> { .id, body: .text } }
}
```

The `meta` of a migration has exactly `name`, the name of the module, and
`from` and `to`, the two versions that it joins. A migration file is read in
the language version of its `to` definition. An `add` or a `remove` that
omits its path applies to its scope's node.

### 15.2 Data changes

A migration states the **data changes** between its two versions: the stored
members, stored sets, and objects that are added, removed, renamed, or
retyped.

The **data structure** of a version is the tree of its stored members after
elaboration: for each body, the names of its stored members, objects, stored
sets, and mount points included, and, for each, its kind, its type with its
cardinality, and the data structure of its body, where a type counts by its
name and by the arguments that its template counts. Applying a migration's
`add`, `remove`, `rename`, and `retype` to the data structure of its `from`
version MUST yield exactly the data structure of its `to` version.

### 15.3 Restated declarations

A `remove` restates the declarations it removes, a `retype` states both types,
and an `add` states the declarations it adds, each as its version declares it.
The names in an `add` body and in a retype's `to` type resolve as in the body
that the path names in the `to` definition, and those in a `remove` body and a
retype's `from` type as in the `from` definition. The names in the `up`
programs and mappings resolve in the `to` definition, and those of `down` in
the `from` definition, as in the body of the node their scope names, the root
at the file level.

### 15.4 Paths

A migration path is a path of the definition, from its top level: `/`, then
the names of the declarations that hold one another, one name per body.
`/tasks/done` is the member `done` in the body of the stored set `tasks`, and
`/archive/items` the stored set `items` in the body of the object `archive`. A
path names declarations, whatever a template does with them: through a
stored set, it goes to a member of its row body, and on to the members inside
that member. Inside `in /path { … }`, a relative path starts at that
declaration, and scopes nest.

- `add` names a body of the `to` version, `remove` a body of the `from`
  version, and `retype` a member of both.
- `rename` names a `from` path and a `to` path that differ in their last name
  once the file's renames of enclosing declarations apply, so a declaration
  keeps its owner.
- The flat and the nested forms mean the same, and mix.
- A path stays within one module.

### 15.5 Programs

`up { … }` and `down { … }` are programs (chapter 10) that run after the
structural change of their direction. At the file level `.` is the new root and
`old` the complete, read-only previous root. Inside a scope they are the scope's
node: for an object, the object of each owner; for a stored set, each row that
the migration carries, `.` its new value and `old` its previous value. `old` has
the type that the other version gives its node. A retype's `up` and `down` map
one value, `old` being the member's previous value, to its new value. Programs
read `.` and `old`, write, and call functions and mutations; mappings read `old`
and call functions. Through a mount point, `.` reaches the instance mounted
there as the mount point's shape presents it, and `old` reaches that instance as
it stood before the migration:

```liasse
meta { name: "acme.app", from: "1.0.0", to: "2.0.0" }

in /users {
  remove { email: Text }
  up { .data.set_email(old.email) }
  down { .email = old.data.email }
}
```

Each user's `data` holds an instance whose shape lists `email: Text` and
`mut set_email(email: Text)`: `up` hands each user's email to that instance, and
`down` takes it back.

### 15.6 Every loss is accounted for

Every migration works in both directions. Every value that leaves state in a
direction, a removed member going up, an added member going down, and a
retype's source values in each direction, is accounted for in that direction:

- by `drop path` in a program of that direction, naming its path or a path that
  contains it;
- by a statement of that direction that reads it through `old`, where reading a
  member accounts for that member's path and the stored sets and objects that
  hold it, and reading a stored set or object as a whole accounts for every
  path under it;
- for a retype, by its mapping.

The children that a removed mount point held are values that leave state. A
`drop` names a path that leaves state in its direction, and a required member
that enters state has an initializer, or a program of that direction writes it.

---

## 16. Libraries

```liasse
//! What the host provides.
fn sort<V, A>(items: V*, key: V -> A = .): items
fn desc<V>(x: V): V
mut rates.quote(pair: Text): { bid: Decimal; ask: Decimal }
mut mail.send(to: Text, subject: Text, body: Text)
mut ticket(): Text

/// The midpoint of a bid and an ask.
fn mid(bid: Decimal, ask: Decimal): Decimal = (bid + ask) / 2.0

/// The rate from `from` to `to`, answered through `rates.quote`.
mut rates.get(from: Text, to: Text): Decimal {
    let quote = rates.quote(pair: from + to)
    return mid(quote.bid, quote.ask)
}
```

A **library** is a file of declarations, and it stands for its entries written
at the root of a module: its entries join the module's root before the module's
names resolve. A **host library** is a library that the host injects into
modules, and it also holds names qualified by a namespace, such as
`rates.quote`, and **opaques**: declarations that are a signature alone, which
the system implements. A signature gives the name, the type parameters, and the
parameters of a declaration, and the result type of a function or a mutation
that has one.

The system gives what a signature leaves open: the set that `sort` yields, and
the value that `desc` yields. `rates.quote` is the mutation `quote` of the
namespace `rates`, called as `rates.quote("EURUSD")` or brought by `use` (14.3),
and `ticket` a mutation called by its name. A mutation whose signature ends with
its parameters, such as `mail.send`, is called as a statement of a program
(10.2).

The declarations of a host library name each other and the builtins, and its
functions and mutations read their parameters and their inputs alone: `mid`
computes from its parameters, and `rates.get` answers through `rates.quote`.

The **builtins** are the declarations of the host libraries that the
implementation of the language version provides: the **host types**, and
templates, mixins, functions, and mutations.

---

## Annex A. Grammar

Chapter 2 defines the tokens and line breaks.

```text
// Files
Module         = InnerDocs? (RootEntry % EntrySep)?                    // a `.liasse` file
RootEntry      = Docs? (Meta | Requires | Use | Declaration)
MigrationFile  = InnerDocs? MigrationMeta (EntrySep MigrationEntry)* EntrySep?  // a `.migration.liasse` file
Library        = InnerDocs? (LibraryEntry % EntrySep)?                 // a `.lib.liasse` or `.sys.liasse` file
LibraryEntry   = Docs? (Declaration | HostEntry)
HostEntry      = Opaque                                                // in a `.sys.liasse` file
               | 'fn' QualName TypeParams? Params ':' Type ('=' Expression | FnBlock)
               | 'fn' QualName NoneParams ':' 'none'
               | 'mut' QualName TypeParams? Params (':' Type)? Program
Opaque         = 'fn' QualName TypeParams? Params ':' Type             // a signature alone
               | 'mut' QualName TypeParams? Params (':' Type)?
               | 'template' QualName TypeParams? Params? | 'mixin' QualName TypeParams? Params?
               | 'fn' TypeParams? OpSignature                          // an operator declaration

Meta           = 'meta' '{' (MetaMember % MemberSep) '}'
MetaMember     = Docs? NAME ':' TEXT
Requires       = 'requires' '{' ((NAME ':' TEXT) % MemberSep)? '}'
Use            = 'use' NAME '.' (UseItem | '{' UseItem % ',' '}' | '*')
UseItem        = NAME ('as' NAME)?

// Declarations
Body           = '{' InnerDocs? (Entry % EntrySep)? '}'
Entry          = Docs? (Declaration | SetLevel | SlotFill)
SetLevel       = 'set' Body                                            // in a definition of cardinality * or +
SlotFill       = NAME Body
Declaration    = Data | View | Mut | Function | MixinApp | Slot | Template | Mixin
               | Public
Data           = NAME ':' Definition ('=' Expression)? | NAME '=' Expression
View           = 'view' NAME TypeParams? Params? (':' Definition)? ('=' Expression)?   // '=' outside a shape
Mut            = 'mut' NAME TypeParams? Params (':' 'view'? Type)? Program?            // Program outside a shape
Function       = 'fn' NAME TypeParams? Params ':' Type ('=' Expression | FnBlock)
               | 'fn' NAME NoneParams ':' 'none'                       // a none overload
FnBlock        = '{' InnerDocs? (FnEntry EntrySep)* 'return' Expression EntrySep? '}'
FnEntry        = Docs? ('let' NAME '=' Expression | Function)
MixinApp       = QualName TypeArgs? '(' Args ')' Body?
Slot           = 'slot' NAME? Body?
Template       = 'template' NAME TypeParams? Params? ':' Definition
Mixin          = 'mixin' NAME TypeParams? Params? Body
Public         = 'public' Body

// Definitions and parameters
Definition     = QualName TypeArgs? ('(' Args ')')? Mark* Body? Mark*  // a data member's: one Card at most
               | Body Mark*
Type           = TypeTerm ('|>' TypeTerm)?
TypeTerm       = QualName TypeArgs? ('(' Args ')')? Mark* | Body Mark* // a definition that ends before any body
Mark           = Card | '!'
Card           = '?' | '*' | '+'
QualName       = NAME ('.' NAME)?
TypeParams     = '<' (TypeParam % ',') '>'
TypeParam      = NAME (':' Type)? | OpEntry
OpEntry        = Type ('+' | '-' | '*' | '/' | '%') Type ':' Type | PrefixOp Type ':' Type
OpSignature    = Type BinaryOp Type ':' Type | PrefixOp Type ':' Type
BinaryOp       = '+' | '-' | '*' | '/' | '%' | '==' | '!=' | '<' | '<=' | '>' | '>=' | 'in'
PrefixOp       = '-' | '!'
TypeArgs       = '<' (((NAME ':')? Type) % ',') '>'                    // all named, or all positional
Params         = '(' (Param % ',')? ')'
Param          = Docs? NAME (':' Kind)? ('=' Expression)?
Kind           = Type '->' Definition | Definition
NoneParams     = '(' ('none' % ',' | (NAME ':' 'none') % ',') ')'      // all named, or all positional

// Programs
Program        = '{' (Statement % EntrySep)? '}'
Statement      = Docs? 'let' NAME '=' (Write | Expression)
               | Write
               | Expression                                            // a call
               | 'return' 'view'? (Write | Expression)
               | Drop                                                  // in a migration program
Write          = Target ('=' | '+=' | '-=') Expression | Target PatchBlock
Target         = Postfix
PatchBlock     = '{' (PatchMember % MemberSep)? '}'
PatchMember    = NAME '=' Expression | '=' Path | NAME PatchBlock

// Expressions
Expression     = Conditional ('|>' Stage)*
Stage          = PipeCall | '.' '->' Conditional | Conditional          // a call when it starts with a name
PipeCall       = QualName TypeArgs? ('(' Args ')')? Record?
Conditional    = Fallback ('?' Expression ':' Conditional)?
Fallback       = Or ('??' Or)*
Or             = And ('||' And)*
And            = Equality ('&&' Equality)*
Equality       = Comparison (('==' | '!=') Comparison)*
Comparison     = Additive (('<' | '<=' | '>' | '>=' | 'in') Additive)*
Additive       = Multiplicative (('+' | '-') Multiplicative)*
Multiplicative = Unary (('*' | '/' | '%') Unary)*
Unary          = ('!' | '-')* Postfix
Postfix        = Primary ('.' NAME | '.' '$' NAME | '[' Selector ']' | TypeArgs? '(' Args ')' | '!')*
Primary        = Literal | Root | Record | Tuple | Set | '(' Expression ')'
Root           = '/' NAME? | '.' NAME? | '.' '$' NAME | '^'+ ('.' NAME)? | '@' NAME | NAME
Path           = Root ('.' NAME)*
Selector       = Expression % ','
Record         = '{' (Member % MemberSep)? '}'
Member         = NAME ':' Expression | Shorthand
Shorthand      = NAME | '@' NAME | '.' NAME | '/' NAME | '^'+ '.' NAME   // a short path
               | '@'? NAME '.' NAME
Tuple          = '(' ')' | '(' Expression ',' ')' | '(' Expression (',' Expression)+ ')'
Set            = '[' (Expression % MemberSep)? ']'
Args           = (Arg % MemberSep)?                                     // all named, or all positional
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
