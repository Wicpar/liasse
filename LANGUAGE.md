# The Liasse Language

Language version `liasse 0.0.1`.

## 1. A first module

A **module** declares stored state, views that read it, and mutations that
change it. An **instance** is a module with its own state.

```liasse
meta { name: "acme.tasks", version: "1.0.0", language: "liasse 0.0.1" }

tasks: {
    pub title: Text
    pub done = false
    pub mut complete() { .done = true; }
}*
pub view open_tasks = .tasks[!.done]
pub mut add(title: Text) {
    return view .tasks += { title };
}
```

`tasks` stores rows, each with a text title and a Boolean initially false.
`open_tasks` selects unfinished rows. `add` inserts a row and returns a live
view of it; `complete` changes its `done` member. `pub` exposes a declaration
through the module's public interface.

Declarations need no terminator. Executable statements end with `;`, including
`return`. Newlines are whitespace, so expressions can span lines. Commas
separate arguments, record fields, set elements, and selection conditions;
these lists allow a trailing comma. Later examples are module fragments.

## 2. Values and state

### 2.1 Values and declarations

A **data member** stores a value. Its type follows `:` and its initializer
follows `=`; an omitted type is inferred. An **object** groups stored members.
Stored declarations form a **schema**, including initializers and constraints.
A **record** is a value with named fields:

```liasse
address: {
    city: Text = "Paris"
    country: Text = "FR"
}
view label = { city: .address.city, country: .address.country }
```

Integers, decimals, text, and Booleans have types `Int`, `Decimal`, `Text`,
and `Bool`. Numbers are exact; `-2` applies negation to `2`. Integers can
widen to decimals. Other host types use declared constructors or conversions.

Text uses `"escaped\ntext"`, `r"raw text"`, or `"""multiline text"""`.
Raw and multiline text preserve their contents. Names are case-sensitive.
Comments use `//` or nesting `/* … */`. Documentation uses `///` or
`/** … */` for the following declaration or entry, and `//!` or `/*! … */`
for the enclosing file or body. Annex A gives token details.

`label` has type `{ city: Text, country: Text }`. Record fields match by
name and can read siblings: `{ low: 10, high: low + 5 }`. Dependencies are
acyclic. Objects read as records of their data and can also hold views and
mutations. An inferred record initializer creates private stored fields.
Parentheses group expressions; records group named values.

### 2.2 Optional values and sets

A type's **cardinality** states how many values it permits at one level:

| Type | Meaning |
| --- | --- |
| `T` | One value. |
| `T?` | Zero or one value. |
| `T*` | A set of zero or more values. |
| `T+` | A set of one or more values. |

A **set** is ordered and allows duplicates. `[1, 2, 1]` has three elements.
Each item contributes one element, preserving its inner structure. A nonempty
literal has type `E+`, where `E` is its items' common type.

A mark gives an unmarked type its cardinality; a further mark adds an outer
level. `Text*?` is an optional set; `Text**` is a set of sets. Stored schemas
have one mark at most, including marks supplied by templates. Expressions and
callable types can nest them.

Interpret **`none`** and **`[]`** using the expected type. **Fallback** `??`
uses its right operand when the left has zero outer values:

| Expected type | Literal | Value | Triggers `??`? |
| --- | --- | --- | --- |
| `Text?` | `none` | Missing text. | Yes. |
| `Text*` | `[]` | Empty set. | Yes. |
| `Text*?` | `none` | Missing set. | Yes. |
| `Text*?` | `[]` | Present empty set. | No. |
| `Text**` | `[]` | Empty outer set. | Yes. |
| `Text**` | `[[]]` | One empty inner set. | No. |

`none` requires an optional outer level. `[]` constructs the outer set,
possibly inside optional wrappers; another set level requires brackets.
`Text* = none`, `Text? = []`, and empty literals addressed to required or
nonempty levels fail type checking.

Annotations, other items, or calls determine element types: `[none, "a"]`
has two `Text?` elements; `[[], ["a"]]` has two inner sets. A bare
`view x = none` or `view x = []` needs more type information.

### 2.3 Fitting

A value **fits** a type when its structure and cardinality meet that type's
requirements:

| Source cardinality | Targets |
| --- | --- |
| One | One, `?`, `*`, `+`. |
| `?` | `?`, `*`. |
| `*` | `*`. |
| `+` | `+`, `*`. |

Thus `view numbers: Int* = 1` reads a singleton set. Fitting uses static
types: a currently nonempty `Int*` still cannot fit `Int+`. Fields and
elements fit recursively; the only primitive widening is `Int` to `Decimal`.

An already typed value becomes one outer element when its whole type fits
the target's element type; otherwise fit corresponding levels. A typed `Text*`
therefore fits `Text*?` as a present set. The literal rules above take priority;
fitting cannot invent a level to admit an incompatible empty spelling.

The **common type** is the least type all candidates fit: `[1, 2.5]` uses
`Decimal`, and `Text` with `Text?` uses `Text?`. Record fields combine
by name; different shapes need an explicit interface. A unique solution is
required.

### 2.4 Initialization and ownership

An initializer runs when its member's owner is constructed. Supplied fields
replace initializers, including with `none` or `[]`. Omitted fields use
initializers, then these defaults:

| Member | Default |
| --- | --- |
| Optional | `none`. |
| Set of cardinality `*` | `[]`. |
| Required object | Construct its members recursively. |
| Other required value or nonempty set | A supplied value is required. |

```liasse
currency = "EUR"
invoices: {
    net: Decimal
    currency = ^.currency
    tax: Decimal = .net * 0.2
    gross = .net + .tax
    note: Text? = "New"
}* = [{ net: 10 }, { net: 20, tax: 0, note: none }]
```

The gross amounts are decimal `12` and `20`; each invoice keeps the currency
copied at construction. A set initializer supplies **seed data**, constructed
like inserted rows.

Initializers read completed siblings and seed rows in acyclic dependency order,
including calls. Supplied fields replace their initializers' dependencies.
Enclosing state is read at construction.

Each owner's stored set has its own **rows**, in insertion order. Rows own
their nested objects and sets. Removing an owner removes its nested state.
Replacing a present object's data keeps its place and lifetime; removing and
reconstructing it starts another lifetime.

### 2.5 Constraints and bodies

A body before a cardinality describes an element. A body after `*` or `+`
describes the whole set:

```liasse
labels: Text {
    assert(. != "", "A label is required")
}* {
    unique()
}
tasks: {
    code: Text
    title: Text
    done = false
}* {
    unique(.code)
}
```

`assert` checks each label; `unique` checks the labels or task codes together.
`.` is the element or set of its body, and `^` its owner. A callback such
as `unique`'s key uses `.` for its supplied element.

A **constraint** must hold after initial construction and at transaction commit,
including required and nonempty cardinalities. Intermediate statements may
break it. Reading a temporarily missing required value fails; a writable path
can still repair it. Input constraints are checked when arguments bind.
An **assertion**, `assert(test, message);` in a program, checks immediately
and fails that program when false.

Element bodies hold constraints, views, mutations, and mixins; object and row
bodies can also hold data. Set bodies hold mixins and template slots. A body
after `?` describes each present element, as one before `?` does.

## 3. Expressions

### 3.1 Names, records, and calls

Scope is lexical, with ordinary shadowing. Declarations allow forward
references; block locals start at their declaration.

| Form | Refers to |
| --- | --- |
| `name` | Nearest lexical binding, then a readable state member. |
| `.name` | A member of the current value. |
| `^`, `^^` | The enclosing value, then one level further out. |
| `/name`, `/.name` | A member of the declaring module's root. |
| `@name` | A view or mutation input, through nested expressions too. |

In `invoices`, `.net` reads that invoice and `^.currency` its owner.
Initializers use their owner as `.`. Selection, mapping, and callbacks bind
`.` to an element and `^` to the surrounding current value. Helpers retain
their declaring environment and module root.

Data, views, mutations, and named slots share member names. Functions,
templates, and mixins share a separate namespace. A bare call chooses a lexical
binding, then a function, then a state callable; `.name()` chooses a member.
Type positions choose the required kind; explicit names precede wildcards.

A record path abbreviates a field of its final name: `{ .address.city }`
means `{ city: .address.city }`. Shorthand accepts lexical names, `@name`,
and member paths rooted in `.`, `^`, or `/`. Records keep the surrounding
current and enclosing values.

Calls supply arguments all by position or all by name. Positional arguments
fill the first parameters; named arguments can appear in any order. Arguments
evaluate once in source order. Parameterless views are read as members.
A trailing record supplies the last value argument:
`f(a) { key: value }` means `f(a, { key: value })`. With named arguments,
exactly one value parameter must remain.

### 3.2 Operators, equality, and fallback

From strongest to weakest binding:

| Operators | Association |
| --- | --- |
| `.name`, `.$name`, call, selection, postfix `!` | Left. |
| Prefix `-`, `!` | Right. |
| `*`, `/`, `%` | Left. |
| `+`, `-` | Left. |
| `<`, `<=`, `>`, `>=`, `in` | Left. |
| `==`, `!=` | Left. |
| `&&` | Left. |
| `\|\|` | Left. |
| `??` | Left. |
| `? … : …` | Right. |
| `\|>` | Left. |

Operators use declared signatures, preferring exact operand types before
integer-to-decimal widening. Ambiguous matches fail. Operands are scalar
unless the signature allows another cardinality, and evaluate in source order
apart from record and default dependencies.

**Equality** compares fitted values. Records and objects compare field names
and data, including private data in internal comparisons. Sets compare elements
in order, with duplicates: `[1, 2] != [2, 1]` and `[1, 1] != [1]`.
`x in items` tests whether some element equals scalar `x`.

An empty literal in a comparison takes its expected type from the other
operand: `Text?` uses `none`, `Text*` uses `[]`, and `Text*?` can test
either case. Two empty literals alone need type context.

Boolean `&&` and `||` short-circuit; prefix `!` negates a Boolean.
`test ? yes : no` evaluates one branch, fitted to their common type.
Fallback `a ?? b` follows the table in 2.2 and evaluates `b` only when
needed. Its nonempty branch unwraps an optional or narrows a set to `+`.
The result uses that branch's common type with `b`, preserving cardinality
precision. An intrinsically empty left side uses the right side's type.

### 3.3 Sets and pipelines

A member read through a set joins its member results in order. For projects
containing tasks, `.projects.tasks.title` reads titles in project order, then
task order. Optional values work the same way. Nested sets flatten until their
elements have the member; stored rows retain their origins (5.2).

A **selection** `items[conditions]` retains elements satisfying every Boolean
condition, preserving order, duplicates, and origins. Commas mean short-circuit
conjunction. Inside the brackets, `.` is the candidate and `^` the value
outside. A named path from a lexical name, `@`, `^`, or `/` abbreviates
equality: `tasks[@code]` means `tasks[.code == @code]`. `.done` remains
a Boolean test.

Selection adds zero to cardinality: one becomes `?`, `+` becomes `*`.
A selection of one owner's stored set becomes `?` when its equalities fully
determine a present unique key. Absent key components or several owners retain
the ordinary cardinality.

A **pipeline** passes a value through stages. `x |> f(a)` means `f(x, a)`;
`x |> f` means `f(x)`. A complete stage consisting of a name or call uses
this form. Named arguments supply the remaining parameters, and a trailing
record supplies the last one.

Every other stage **maps** an expression over the elements. `. -> e`
explicitly requests mapping, including for a call or bare bound name:

```liasse
numbers: Int* = [1, 2, 3]
view doubled = .numbers |> . -> . * 2
view pairs = .numbers |> { value: ., next: . + 1 }
view groups = .numbers |> [., . + 10]
view flat = (.numbers |> [., . + 10])!
```

Mapping retains each entire result at the source's outer cardinality; scalar
sources give one result. `groups` has three inner sets; `flat` is
`[1, 11, 2, 12, 3, 13]`. Pipeline calls use their callee's result type.
The next `|>` ends a stage; group an inner pipeline in parentheses.

`S |> U` is the type of mapping a `U` over `S`: `U` for scalar `S`,
otherwise `U` with `S`'s outer mark added. `Int* |> Text+` is `Text+*`.
**Flattening** `x!`, with type `T!`, joins one inner level in order. The
result allows zero if either level does, and many if either does.
`Text+*!` is `Text*`; `Text++!` is `Text+`. Scalar or singly marked
values stay as they are. An absent inner value contributes zero elements.

A **metaproperty**, `x.$name`, is a read-only host observation, read through
sets like a member. A host can supply `row.$history`; a record includes it
by explicitly reading it into a field.

## 4. Functions and views

### 4.1 Declarations and inputs

A **function** has a written result type and an expression or block body:

```liasse
fn greeting(name: Text, prefix: Text = "Hello"): Text = prefix + " " + name
fn twice(number: Int): Int {
    let result = number * 2;
    return result;
}
```

A block holds locals and functions, ending in one `return`. Functions capture
their environment and can live at the root, in function blocks, or as template
and mixin helpers. Block functions become available at their declaration.

A **view** reevaluates an expression when read. Its result is inferred or
annotated: `view count_open: Int = count(.open_tasks)`. A parameterless view
using only literals, immutable calls, and other **constant views** is itself
constant and usable at build time.

Parameters can have constraint bodies, as in
`title: Text { assert(. != "", "Required") }`, using the element/set
placement rule. Defaults read parameters and constants once in acyclic
dependency order; supplied arguments replace them.

Functions, templates, and mixins default trailing parameters; views and mutations
can default any parameter. Omitted view or mutation inputs also default to
`none` for `?` and `[]` for `*`. Defaults make immutable calls, with stable
and drawn calls also allowed for mutations. Used defaults participate in inference.

A view or mutation can introduce an input with **`@name`**. Its header can
also name it, with a type, default, or check:

```liasse
title = "Inbox"
mut rename() { .title = @title; }
mut rename_again(title) { .title = title; }
```

Both infer `Text`. Mutation headers can infer input types from uses; function
headers give types. Assignments, comparisons, selections, operators, calls,
and construction fields supply expected types, including through locals.
An input must fit all its uses: one written to `Int` and `Decimal` is `Int`.
Unconstrained inputs need a header type. New `@` inputs follow explicit
parameters in first-use order.

### 4.2 Callbacks and generics

A **callback** parameter `V -> T` accepts an expression computing a `T`
from a `V`. `. -> e` requests a callback explicitly:

```liasse
fn choose(items: Int*, keep: Int -> Bool): Int* = items[keep(.)]
view positive = choose([-1, 0, 2], . > 0)
view all = choose([-1, 0, 2], . -> true)
```

The callee invokes `keep(value)`. Callbacks capture call-site bindings and
obey both the captured and application contexts' call restrictions.

A **type parameter** is inferred from a call; bounds state required members
or operators:

```liasse
fn either<V>(first: Bool, a: V, b: V): V = first ? a : b
fn unpaid<V: { paid: Bool }>(items: V*): items?! = items[!.paid]
fn double<A + A: A>(x: A): A = x + x
```

`either(true, "a", "b")` infers `V = Text`; `double(2)` infers `A = Int`.
Operator bounds introduce their new type names. Bodies must work for every
specialization their bounds permit.

Inference combines arguments, callbacks, an expected result, and any implicit
receiver. `V` gathers whole argument types; `V*` gathers element types, or
the type of a scalar argument. Their common type must meet the bounds.

A parameter name used as a type denotes the argument's type before promotion,
bounded by its written type. The binding retains that specialization, so
`unpaid` keeps extra fields and origins; `items?!` adds zero to its outer
cardinality.

Empty literals contribute their optional or set kind. `either(true, none,
"a")` infers `Text?`. Any type used by an operation or escaping into a
result must be determined: identity on `[]` needs an expected set type,
whereas `count([])` can leave its unused element type undetermined.

Calls infer type arguments. Type applications write them by position or name,
as `Map<Text, Int>` or `Map<K: Text, V: Int>`, or infer them from values.

### 4.3 Missing and empty function cases

A function can add **cases** to its normal declaration:

```liasse
fn share(part: Decimal, whole: Decimal): Decimal = part / whole
fn share(part: none): none

fn labels(items: Int+): Text+ = items |> . -> "item"
fn labels(items: none): none
fn labels(items: []): []
```

`none` matches a missing optional input; `[]` matches a present empty set.
A `none` case permits outer absence, adding an optional wrapper when needed.
A `[]` case extends a nonempty set from `+` to `*`.
A case's result is likewise missing (`: none`) or an empty set (`: []`).
For a host `sum(items: Int+): Int`, `fn sum(items: []): none` returns a
missing integer for an empty input. A function mapping `Int+` to `Int+`
can instead declare `fn doubled(items: []): []`.

Designate parameters all by name or all by position. Positional markers name
the first parameters: `fn share(none): none` designates `part`. A case
applies when **any** designated argument matches its marker. Arguments fit
the combined contract and, with defaults, are evaluated and bound before
dispatch, using bound typed values. A matching case skips the normal body.
Otherwise, the arguments fit the normal contract, with any optional wrapper
introduced by a case unwrapped.

Infer generics from argument branches reaching the normal body, then include
case results permitted by the original argument types:

| Argument to `labels` | Result |
| --- | --- |
| `Int+` | `Text+` |
| `Int*` | `Text*` |
| `Int+?` | `Text+?` |
| `Int*?` | `Text*?` |

A `none` result adds optionality to the whole normal result, retaining an
existing outer optional level. A `[]` result allows an empty set at the
result's set level, possibly inside optional wrappers. Markers must be compatible
with the normal contract; other generic relationships remain intact.

Cases with different results must be disjoint: missing and empty on one
parameter are disjoint, but missing `a` and empty `b` can coincide.
Conflicting cases fail at declaration; agreeing cases may overlap. Cases
share the normal function's visibility and have no priority order.

### 4.4 Permitted calls

A callable's **call class** is immutable (arguments and fixed configuration),
stable (the read environment), drawn (a result beyond that environment),
state-changing (writes), or effect (an external action). Source functions
inherit immutable or stable behavior. Mutations inherit their strongest behavior;
writes and effects count as state-changing for their callers. Defaults,
transitive calls, and callbacks participate in classification.

Calls and view dependencies must be acyclic, including through defaults,
callbacks, and module interfaces.

| Context | Permitted calls |
| --- | --- |
| Build-time argument | Immutable functions and immutable mutations with results. |
| Constant view | Immutable functions. |
| Function, view, read callback, constraint | Immutable and stable functions. |
| Initializer, mutation default | Immutable and stable functions; immutable, stable, and drawn mutations. |
| Mutation program | Every class. |
| Migration mapping | Immutable and stable functions. |
| Migration program | Calls whose transitive calls perform zero effects. |

## 5. Mutations

### 5.1 Statements and writes

A **mutation** runs statements in order over the intermediate state. `let`
binds a result for following statements. A final `return` supplies the result;
omitting it gives no result. A program needs at least one statement.
Writes target the declaring instance; mutation calls can reach other instances.

| Write | Meaning |
| --- | --- |
| `target = value;` | Assign a member, replace rows, or put through an equality selection. |
| `rows += value;` | Insert fresh rows in operand order. |
| `rows -= value;` | Remove every row equal to any operand element. |
| `target { a = value, b = value };` | Patch members together. |

Object and row writes construct values using the supplied-member and default
rules of 2.4. `none` removes an optional object and its owned state; `[]`
clears a set. Scalar `+=` and `-=` use ordinary arithmetic.
Insertion appends one row per outer element, preserving duplicates unless a
constraint rejects them. Removal compares complete data by value, even when
supplied original rows, and returns their snapshots from before removal.

A write yields what it writes: the assigned value, inserted or replaced rows,
or patched receivers. Insertion and replacement have the operand's outer
cardinality; removal returns `*`. Writes can supply a `let`, return, or the
whole right side of another assignment. An inner write runs first and supplies
its result; ordinary operand reads use the statement's starting state.

A state-changing call occupies these same positions. Other expression statements
are mutation calls or assertions. Nested calls share the transaction;
failure discards all its changes.

### 5.2 Origins, replacement, and put

A row's **origin** identifies it for its lifetime. Paths, selections,
row-preserving reordering, and mappings returning original rows keep origins.
Parameters, locals, and constructed fields holding original rows can therefore
reach writable state. Scalar copies and snapshots are values rather than aliases.

Targets resolve against the statement's starting state. Each reached row is
written once; incompatible requests through repeated targets fail. Assigning
a row replaces its data and retains its origin. Assigning a set matches input
rows by their origins in that set, then new values by declared unique keys.
Matched rows retain their origins and positions; unmatched old rows are removed,
and fresh rows append in operand order. Replacement yields stored order.
Repeated origins, ambiguous keys, and conflicting origin/key matches fail.

A **put** assigns through an equality selection:

```liasse
limits: {
    key: Text
    value: Int = 0
}* { unique(.key) }
mut set_limit(key: Text, value: Int) {
    .limits[.key == key] = { value };
}
```

Put replaces each match or inserts one row when there are zero matches.
Conditions are equalities on distinct direct stored members, or selection
shorthand; they supply omitted fields. A repeated field in the operand must
agree. Scalar rows use `. == value`. The operand describes one row. The
result has cardinality `+`, or one for a fully present unique key.
Other Boolean selections support member writes and patches; other row-producing
expressions support replacement of their existing rows.

### 5.3 Patches and calls

A **patch** writes named members together, each once. Its right sides and
nested patches read the statement's starting state:

```liasse
pair: {
    left: Int = 1
    right: Int = 2
}
mut swap() { .pair { left = .pair.right, right = .pair.left }; }
```

Nested patches require present objects. Patches keep surrounding lexical names
and current values; row mutations supply per-row expressions. Shorthand `=path`
uses the final name: `{ =@locale }` means `{ locale = @locale }`.

`receiver.mutation(args)` runs once per distinct receiver origin. Arguments
are evaluated once. Each run starts in the statement's starting state and sees
its own writes; conflicting changes to shared state fail their combination.
Results join in receiver order.

A patch attached to a mutation call runs after it, once, in the same transaction,
reading the state it leaves. The call must return a view.
`f(a) { field: value }` supplies a record argument;
`f(a) { field = value };` patches its result. An empty direct block is a
record argument; `(f(a)) {};` requests an empty patch.

### 5.4 Returned results

`return e;` produces a **snapshot**, preserving its data through later changes.
`return view e;` produces a **returned view**, reevaluating its expression:

```liasse
tasks: {
    pub title: Text
    pub done = false
    pub mut complete() { .done = true; }
}*
pub mut add_snapshot(title: Text) {
    let task = .tasks += { title };
    return task |> { .title, .done };
}
pub mut add_live(title: Text) {
    return view .tasks += { title };
}
mut add_completed(title: Text) {
    .add_live(title) { done = true };
}
```

The snapshot keeps the inserted values. The live result reads current data
and exposes `complete`. Captured parameters and ordinary locals keep their
values; captured rows keep origins, and removed rows leave the view. Objects
keep their place and lifetime. Reevaluating a path can observe a newly constructed
object where a captured, removed object has left the view.

A directly returned write runs once. A returned-view expression can also begin
with a state-changing call followed by read-only access, selection, or a pipeline,
as in `return view .add_live(title) |> { .title };`: the call runs once,
its continuation stays live. Use a local to continue reading a write's result.
Captured snapshots stay snapshots.

A result annotation `: T` requests a snapshot; `: view T` requests a view.
The return must agree. Omitting the annotation infers type and mode. Public
results use the interfaces in 7.2.

## 6. Templates and mixins

A **template** names a schema or interface, optionally parameterized by types
and build-time values. A **mixin** contributes declarations to its application
body. **Slots** accept declarations from an application:

```liasse
mixin named(fallback: Text = "Untitled") {
    pub title: Text = fallback
}
template Cards(label: Text = "Card"): {
    named(label)
    slot { pub note: Text = "" }
    details: { slot fields { pub priority: Int = 0 } }
}* {
    slot checks
}
cards: Cards("Task") {
    pub done = false
    fields { pub owner: Text = "ana" }
    checks { unique(.title) }
}
```

Bare entries fill the unnamed slot: `done` replaces `note`. Named fills
can reach nested slots: `owner` replaces `priority`, and `unique` lands
in the set body. Omitted slots use their fallback; an empty fill removes it.
Each template or mixin owns at most one slot of each name, including unnamed,
and each slot accepts one fill. Nested definitions own their own slots.

Definitions keep their lexical bindings; supplied arguments and fills keep
those of the application. Argument paths refer to the application site;
definition and slot bodies take state levels from their destination and its owners.
Applications create fresh declarations, preserving documentation and `pub`.
Template configuration is fixed at build time. Mixin expressions run at their
uses; untyped parameters infer from those uses, or require a written kind.

Nested templates and mixins are available to their enclosing definition and
its slots; those inside a fallback are available to that slot alone. Such
helpers keep their lexical environment and must not conflict with application
bindings. Functions retain their helper scope after expansion.

Templates can recurse through optional members or empty sets:

```liasse
template Node: {
    pub label: Text
    pub children: Node*
}
tree: Node = { label: "Root", children: [] }
```

Construction must terminate, with finitely many specialized schemas. Mixin
expansion and fresh inline declarations must be acyclic. In type positions,
templates contribute structure and checks; stored construction also initializes.

## 7. Modules and interfaces

### 7.1 Files and imports

A module starts with metadata, then optional requirements and imports, then
declarations. Its library files share its root scope:

```liasse
meta { name: "acme.tasks", version: "1.0.0", language: "liasse 0.0.1" }
requires { text: "acme.text@1" }
use text.slugify
use text.Slug as TaskSlug
```

Metadata has exactly these three fields, in any order. Module names are
dot-separated components of lowercase ASCII letters, digits, and underscores,
each starting with a letter. Versions are three canonical nonnegative integers,
compared numerically; `language` selects the language version. One `requires`
block binds distinct namespaces to module names and major versions.

`use ns.name` imports a public declaration; `as local` renames it.
`use ns.{ name, Other as Local }` imports a list; `use ns.*` adds wildcard
candidates. Explicit imports must differ from root declarations; conflicting
wildcards need qualification. Imports preserve identity and environment.
Hosts can also supply namespaces.

### 7.2 Public and structural interfaces

**`pub`** exposes a declaration across its module boundary. Members are private
by default; module code can access them. Functions, templates, and mixins can
be public at the root. Generated members keep their own modifiers.

A **structural interface** requires named readable members and callable
signatures, allowing extras. A readable member can be stored data or a
parameterless view. Implementations accept every permitted argument and
satisfy result guarantees, including names, checks, defaults, callback kinds,
and snapshot or view mode. Interface-typed parameters expose that interface;
generic bounds retain the argument's actual type.

```liasse
tasks: {
    pub title: Text
    secret: Text = ""
    pub mut complete() { .secret = "completed"; }
}*
pub view open_tasks = .tasks
pub view headings: { title: Text }* = .tasks
pub view labels = .tasks |> { .title, state: .secret }
```

`open_tasks` exposes `title` and `complete`; `headings` exposes only `title`.
Constructed records expose their written fields, so `labels` exposes `state`.
Public results recursively project stored values to public members, including
stored values inside constructed fields. Explicit interfaces can narrow this
surface. Named schema types retain their members' modifiers.

Snapshots expose data only; copy a computed view into a field to include it.
View results can expose views and mutations. External writes call public
mutations through a currently exposed view.

### 7.3 Mounted modules

A `Module` member holds a child instance, with cardinality one or `?`, and
lists its required interface. Its `provide` body implements the parent
interface requested by the child's single required root `Parent` member:

```liasse
// Child declarations.
user: Parent { view region: Text, mut notify(message: Text) }
email: Text = ""
pub view home = .user.region
pub mut set_email(email: Text) {
    .email = email;
    .user.notify("The email changed");
}
```

A parent can declare:

```liasse
users: {
    region: Text
    notices: { message: Text }*
    data: Module {
        view home: Text,
        mut set_email(email: Text),
        provide {
            pub view region = ^.region
            pub mut notify(message: Text) { ^.notices += { message }; }
        }
    }?
}*
```

Both implementations must be public and satisfy their interfaces. In `provide`,
`.` is the mount point and `^` its owner. Provided views read the parent at
the child's read context; mutations share the transaction. Host operations
initialize, mount, and unmount instances.
`Module` and `Parent` require interface bodies; `Parent` holds no state.

### 7.4 Host libraries

**Host libraries** can declare qualified names and **opaque** signatures
implemented by the host, alongside ordinary source definitions:

```liasse
fn text_date(text: Text): Date
fn count<V>(items: V*): Int
mixin unique<V, K>(key: V -> K = .)
mut ticket(): Text
mut mail.send(to: Text, subject: Text, body: Text)
```

The host contract defines meaning, call class, and any implicit receiver.
For `unique`, the set supplies `V` and the callback supplies `K`.
Host types define their values, equality, operations, and configuration.
Operators have signatures such as `fn Int + Int: Int`.

Unless specified otherwise, opaque functions are immutable, mutations returning
values are drawn, and resultless mutations are effects. **Builtins** are the
host declarations supplied for the language version. Injected declarations
occupy their names in the module.

## 8. Migrations

A **migration** describes structural edits and value transformations between
two versions, in both directions. Its metadata names the module and versions;
it uses the target's language version. A **declaration path**, `/tasks/title`,
names stored declarations through their owners within one module.

```liasse
meta { name: "acme.tasks", from: "1.0.0", to: "2.0.0" }
in /tasks {
    add { notes: Text? }
    remove { draft: Text? }
    rename name to title
    retype status from Text to Bool {
        up: old == "done",
        down: old ? "done" : "open"
    }
    up { drop draft; }
    down { drop notes; }
}
```

Here old tasks have `name: Text`, `draft: Text?`, and `status: Text`.
The migration retains rows, renames `name`, exchanges `draft` for `notes`,
and converts `status` between text and Boolean.

| Entry | Meaning |
| --- | --- |
| `in /path { … }` | Scope relative paths and programs to a declaration. |
| `add { … }`, `remove { … }` | Restate declarations entering or leaving on upgrade. |
| `rename path to path` | Rename within one owner, preserving origins. |
| `retype path from T to U { up: e, down: e }` | Transform values in each direction. |
| `up { … }`, `down { … }` | Run statements in that direction. |

`add` and `remove` can put a path before their body; otherwise they use the
enclosing scope or root. Nested scopes compose; paths account for enclosing
renames. Moving values between owners uses programs with add/remove entries.
Edits must reproduce the target's stored structure exactly, including ownership,
types, cardinalities, declaration identities, and significant configuration.
Add bodies, target types, and up code resolve in the target version; remove
bodies, source types, and down code resolve in the source version.

A retype mapping reads **`old`**, the previous member value, once per owner.
Programs read entering state through `.` and a fixed previous snapshot through
`old`, at the scoped root, object, or carried row. A new owner has `old = none`.

Structural changes and mappings precede initialization, then programs run in
file order. Scoped programs run per entering owner in set order. Required
entering values need an initializer, mapping, or write before being read or
finishing the step. The final state satisfies its constraints; transitive calls
perform zero effects.

Every leaving declaration must be mapped, read through `old`, or acknowledged
with `drop path;`. A scalar read accounts for its containing owners too;
reading or dropping a whole object or set includes stored descendants.
`drop a, b;` names several paths. Accounting includes called source bodies
and is checked in both directions.

Removing a mount also accounts for its child instance. Surviving mounts expose
the entering public interface; `old` exposes previous read-only data. Programs
can transfer data through public child mutations.

## Annex A. Complete grammar

`A | B` means alternatives, juxtaposition means sequence, `[A]` is optional,
`{A}` repeats zero or more times, and `A % ','` is a nonempty comma-separated
list. Quotes identify source tokens; unquoted brackets and braces are notation.
Capitalized lexical names denote tokens. Whitespace separates tokens and has
no statement-termination role.

### A.1 Tokens

Source is UTF-8. Names are case-sensitive ASCII names; numeric member names
are invalid. A physical newline is LF, CRLF, or CR. Whitespace also includes
spaces and horizontal tabs. Operators take the longest token: `a!=b` uses
`!=`, so `T! = e` needs space before `=`. In type suffix positions the marks
are read individually, as in `Text*?`.

```text
NAME    = letter {letter | digit | '_'}
INT     = digit {digit}
DECIMAL = digit {digit} '.' digit {digit}
letter  = 'A'…'Z' | 'a'…'z'
digit   = '0'…'9'
```

`TEXT` is ordinary quoted text, triple-quoted multiline text, or `r"…"` raw
text. Ordinary text ends before a physical newline and accepts `\n`, `\r`,
`\t`, `\\`, `\"`, and `\u{h…}` with one to six hexadecimal digits naming
a Unicode scalar value. Invalid escapes fail to parse. Raw text ends at the
next quote before a newline. Multiline text ends at the next triple quote;
its newlines and indentation are preserved. Delimiters are excluded.

`true`, `false`, and `none` are literal tokens. Other words are contextual:
`view name = e` declares a view, while `view = e` declares data. After a
member-access dot, a contextual word is a member name. Complete declaration
or statement forms take precedence at their boundaries; `return(e);` is a
return, while `.return(e);` calls a member. Postfix and operator tokens
continue an expression across whitespace, including newlines.

Comments separate tokens: `//` ends at the newline; `/* … */` nests.
`DOC_LINE` and `DOC_BLOCK` are `///` and `/** … */`; `INNER_DOC_LINE` and
`INNER_DOC_BLOCK` are `//!` and `/*! … */`. A run of documentation attaches
to the next declaration, parameter, record field, statement, or migration
entry in that list, across ordinary comments and blank lines. A run with no
following eligible form is invalid. Inner documentation precedes the enclosing
file or body's first entry. Associations survive expansion and interface
projection.

### A.2 Files and declarations

```text
Module = [InnerDocs] [Docs] Meta [[Docs] Requires]
         {[Docs] Use} {[Docs] RootDeclaration}
Meta = 'meta' '{' MetaMember % ',' [','] '}'
MetaMember = [Docs] NAME ':' TEXT
Requires = 'requires' '{' [Requirement % ',' [',']] '}'
Requirement = [Docs] NAME ':' TEXT
Use = 'use' NAME '.' (UseItem | '{' UseItem % ',' [','] '}' | '*')
UseItem = NAME ['as' NAME]
Library = [InnerDocs] {[Docs] (RootDeclaration | HostEntry)}

RootDeclaration = Data | View | Mutation | Function | FunctionCase
                | Template | Mixin | MixinApplication
Body = '{' [InnerDocs] {[Docs] BodyEntry} '}'
BodyEntry = RootDeclaration | Slot | SlotFill
SetBody = '{' [InnerDocs] {[Docs] SetEntry} '}'
SetEntry = MixinApplication | Slot | SlotFill
Data = ['pub'] NAME ':' Schema ['=' Expression]
     | ['pub'] NAME '=' Expression
Schema = SchemaHead [Body] [Card] [SetBody]
       | SchemaHead '?' Body
       | Body [Card] [SetBody]
       | MountSchema | ContextSchema
SchemaHead = QualName [TypeArgs] [TypeConfig]
MountSchema = 'Module' [TypeConfig] ['?'] MountBody ['?']
ContextSchema = 'Parent' ContextBody
MountBody = '{' [InnerDocs] [MountEntry % ',' [',']] '}'
MountEntry = TypeMember | [Docs] 'provide' ProvideBody
ContextBody = '{' [InnerDocs] [ContextMember % ',' [',']] '}'
ContextMember = [Docs] ViewSignature | [Docs] MutationSignature
ProvideBody = '{' [InnerDocs] {[Docs] (View | Mutation)} '}'

View = ['pub'] 'view' NAME [TypeParams] [Params] [':' Type] '=' Expression
Mutation = ['pub'] 'mut' NAME [TypeParams] Params
           [':' ['view'] Type] Program
Function = ['pub'] 'fn' NAME [TypeParams] Params ':' Type FunctionBody
FunctionBody = '=' Expression | FnBlock
FnBlock = '{' [InnerDocs] {FnEntry}
          [Docs] 'return' Expression ';' '}'
FnEntry = [Docs] 'let' NAME '=' Expression ';'
        | [Docs] Function | [Docs] FunctionCase
FunctionCase = 'fn' NAME CaseParams ':' EmptyMarker
CaseParams = '(' (EmptyMarker % ',' | (NAME ':' EmptyMarker) % ',') [','] ')'
EmptyMarker = 'none' | '[' ']'

Template = ['pub'] 'template' NAME [TypeParams] [Params]
           ':' (Schema | Type)
Mixin = ['pub'] 'mixin' NAME [TypeParams] [Params] Body
MixinApplication = QualName CallArguments [Body]
Slot = 'slot' [NAME] [Body]
SlotFill = NAME Body
```

A schema has at most one cardinality mark, including a template's mark.
`SetBody` follows an explicit `*` or `+`. A named template's application
body fills its slots. Slot content landing in a set body expands to the
entries permitted there. The two optional marks in `MountSchema` allow
placement on either side of its body, with at most one written.

Names within a namespace are distinct, except for a function and its cases.
Parameters and locals in one block are distinct. Calls bind each supplied
argument once to an existing parameter and supply every required argument.
Case designations name distinct existing parameters; positional designations
cannot exceed their number. Empty result markers must fit the normal result's
optional or set structure after the extensions described in 4.3.

Stored data belongs at the root or in an object or row body. Functions belong
at the root, in function blocks, or in template/mixin helper scopes. Nested
templates and mixins belong to templates, mixins, or slot fallbacks. Views and
mutations with bodies belong to state; signatures belong to interfaces or host
libraries. Publication of helpers is valid only at the root after expansion.

### A.3 Types and contracts

```text
QualName = NAME ['.' NAME]
Card = '?' | '*' | '+'
Type = TypeTerm ['|>' TypeTerm]
TypeTerm = TypeAtom {TypeSuffix}
TypeAtom = QualName [TypeArgs] [TypeConfig] | TypeBody
TypeSuffix = Card | '!'
TypeConfig = '(' [Args [',']] ')'
TypeArgs = '<' (Type % ',' | (NAME ':' Type) % ',') [','] '>'
TypeBody = '{' [InnerDocs] [TypeMember % ',' [',']] '}'
TypeMember = [Docs] DataSignature | [Docs] ViewSignature
           | [Docs] MutationSignature
DataSignature = NAME ':' Type
ViewSignature = 'view' NAME [TypeParams] [Params] ':' Type
MutationSignature = 'mut' NAME [TypeParams] Params [':' ['view'] Type]

TypeParams = '<' TypeParam % ',' [','] '>'
TypeParam = NAME [':' Type] | OperatorBound
OperatorBound = Type BinaryOp Type ':' Type | PrefixOp Type ':' Type
Params = '(' [Param % ',' [',']] ')'
Param = [Docs] NAME [':' Kind] ['=' Expression]
Kind = InputType | Type '->' InputType
InputType = CheckedTerm ['|>' CheckedTerm]
CheckedTerm = TypeAtom [CheckBody] {TypeSuffix [CheckBody]}
CheckBody = '{' [InnerDocs] {[Docs] MixinApplication} '}'

HostEntry = HostFunctionSignature | HostMutationSignature
          | HostTemplateSignature | HostMixinSignature
          | HostOperatorSignature | HostFunction | HostMutation
          | HostFunctionCase
HostFunctionSignature = 'fn' QualName [TypeParams] Params ':' Type
HostMutationSignature = 'mut' QualName [TypeParams] Params
                        [':' ['view'] Type]
HostTemplateSignature = 'template' QualName [TypeParams] [Params]
HostMixinSignature = 'mixin' QualName [TypeParams] [Params]
HostOperatorSignature = 'fn' [TypeParams] OperatorSignature
HostFunction = 'fn' QualName [TypeParams] Params ':' Type FunctionBody
HostMutation = 'mut' QualName [TypeParams] Params
               [':' ['view'] Type] Program
HostFunctionCase = 'fn' QualName CaseParams ':' EmptyMarker
OperatorSignature = Type BinaryOp Type ':' Type | PrefixOp Type ':' Type
BinaryOp = '+' | '-' | '*' | '/' | '%' | '==' | '!='
         | '<' | '<=' | '>' | '>=' | 'in' | '&&' | '||'
PrefixOp = '-' | '!'
```

Explicit type arguments occur in type applications and declarations; callable
calls infer them. In operator bounds the required operator ends its left type;
`+` or `*` followed by an operand is that operator. Check bodies before a
set mark check elements; those immediately after `*` or `+` check the set.
A mixed body of unimplemented signatures and executable state declarations is
invalid. A snapshot interface lists data signatures only.

### A.4 Expressions and programs

```text
Expression = Conditional {'|>' Stage}
Stage = PipeCall | '.' '->' Conditional | Conditional
PipeCall = QualName [CallArguments] [Record]
Conditional = Fallback ['?' Conditional ':' Conditional]
Fallback = Or {'??' Or}
Or = And {'||' And}
And = Equality {'&&' Equality}
Equality = Comparison {('==' | '!=') Comparison}
Comparison = Additive {('<' | '<=' | '>' | '>=' | 'in') Additive}
Additive = Multiplicative {('+' | '-') Multiplicative}
Multiplicative = Unary {('*' | '/' | '%') Unary}
Unary = PrefixOp Unary | Postfix
Postfix = Primary {'.' NAME | '.' '$' NAME | CallArguments [Record]
                  | Selection | '!'}
Primary = Literal | Root | Record | Set | '(' Expression ')'
Root = NAME | '@' NAME | '.' [NAME] | '.' '$' NAME
     | '^' {'^'} | '/' [NAME]
Literal = INT | DECIMAL | TEXT | 'true' | 'false' | 'none'
Selection = '[' Expression % ',' [','] ']'
Record = '{' [RecordMember % ',' [',']] '}'
RecordMember = [Docs] NAME ':' Expression | [Docs] Shorthand
Shorthand = ShorthandRoot {'.' NAME}
ShorthandRoot = NAME | '@' NAME | '.' NAME | '^' {'^'} '.' NAME | '/' NAME
Set = '[' [Expression % ',' [',']] ']'
CallArguments = '(' [Args [',']] ')'
Args = Argument % ',' | (NAME ':' Argument) % ','
Argument = Lambda | Expression
Lambda = '.' '->' Expression

Program = '{' [InnerDocs] Statement {Statement} '}'
Statement = [Docs] ('let' NAME '=' ProgramValue
          | Write | Expression | 'return' ['view'] ProgramValue) ';'
ProgramValue = Write | Expression
Write = Target AssignOp ProgramValue | Target Patch
AssignOp = '=' | '+=' | '-='
Target = Postfix
Patch = '{' [PatchMember % ',' [',']] '}'
PatchMember = [Docs] NAME '=' Expression | [Docs] '=' Shorthand
            | [Docs] NAME Patch
```

A complete stage matching `PipeCall` uses the call form; the explicit lambda
and other expressions map. Selection shorthand has the root restrictions of
3.3. A plain program expression must be a mutation call or assertion; writes
require writable targets. Assignment associates right, and return is final.
A direct trailing record takes precedence over an empty patch. Returned-view
calls and their read-only continuations obey 5.4.

### A.5 Migrations

```text
MigrationFile = [InnerDocs] [Docs] Meta {[Docs] MigrationEntry}
MigrationBody = '{' [InnerDocs] {[Docs] MigrationEntry} '}'
MigrationEntry = 'in' MigrationPath MigrationBody
               | 'add' [MigrationPath] Body
               | 'remove' [MigrationPath] Body
               | 'rename' MigrationPath 'to' MigrationPath
               | 'retype' MigrationPath 'from' Type 'to' Type MappingBody
               | 'up' MigrationProgram | 'down' MigrationProgram
MappingBody = '{' [Docs] 'up' ':' Expression ','
                 [Docs] 'down' ':' Expression [','] '}'
MigrationPath = '/' [NAME {'/' NAME}] | NAME {'/' NAME}
MigrationProgram = '{' [InnerDocs] MigrationStatement {MigrationStatement} '}'
MigrationStatement = Statement | [Docs] 'drop' MigrationPath % ',' ';'
Docs = Doc {Doc}
InnerDocs = InnerDoc {InnerDoc}
Doc = DOC_LINE | DOC_BLOCK
InnerDoc = INNER_DOC_LINE | INNER_DOC_BLOCK
```

Module metadata has `name`, `version`, and `language`; migration metadata
has `name`, `from`, and `to`. Paths, types, declaration placement, and the
other conditions in the preceding chapters also apply to these productions.
