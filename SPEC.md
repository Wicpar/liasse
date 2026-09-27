# Liasse v0.5 — Consolidated Specification

Status: **standard draft**.

## Abstract

Liasse is a Rust-first application state engine. A module declares typed logical state together with the views that read it, the constraints that every committed state satisfies, the mutations that change it, and the roles that grant its views and mutations to clients. Modules compose as values: a module mounts child modules at typed mount points and reaches each one only through its interface. This specification defines values, identity, constraints, authorization, atomic admission, ordering, history, migrations, and client-visible coherence.

<a id="scope"></a>

## 1. Scope

This document specifies:

- the module definition, the `.liasse.zip` format that carries it as a package or a dehydrated module, and their identities;
- typed logical state: tables, row identity, uniqueness, constraints, templates, and mixins;
- expressions, functions and Calls, views, and mutations;
- contexts, roles, authentication, sessions, the wire protocol, and live views;
- modules as values: interfaces, mount points, mounting, updating, and unmounting;
- host namespaces, keyrings and key classes, blobs and storage classes, history, migrations, and deletion;
- runtime admission, ordering, completion, and replay semantics;
- the contract between a Liasse runtime, the root client that embeds it, and its Rust host components.

The specification governs observable application behavior.

[LANGUAGE.md](LANGUAGE.md) specifies the syntax and the static meaning of every form that this document uses. The informative Annex D lists the features that come in later stages.

<a id="conformance"></a>

## 2. Conformance and document conventions

### 2.1 Conformance classes

This specification defines four conformance classes:

- A **module definition** conforms when its source satisfies the syntax, typing, identity, requirement, and static validation rules of the language version that its `meta.language` declares (LANGUAGE §13.2).
- A **runtime** conforms when it loads conforming module definitions and preserves every normative state, execution, history, authorization, and client-coherence rule in this document.
- A **client binding** conforms when its sessions, calls, subscriptions, retries, and completion behavior preserve the protocol semantics defined in §11.
- A **host component** conforms when it implements the typed contract it registers, such as a namespace, a key provider, or a blob connector (§13.4, §14.5, §15.6).

A runtime claiming conformance to this specification MUST implement the complete normative language of the language version `liasse 0.0.1`, as this document and LANGUAGE.md define it.

### 2.2 Normative language

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **SHOULD NOT**, **RECOMMENDED**, **MAY**, and **OPTIONAL** are to be interpreted as described in BCP 14 when, and only when, they appear in all capitals.

### 2.3 Informative material

Annex D and the examples and notes of the sections are informative. The other annexes are normative.

### 2.4 Core terminology

An **application** is one logical Liasse state: its root module instance, the module instances mounted in it recursively, and their retained history. A **module definition** is the source of one module: one `.liasse` file together with the libraries it requires (§3.2). A **module instance** is one module value: the root module of the application, or a module mounted at a mount point (§12.3). It comprises its active definition, its owned state, its history, and the module instances mounted in it. **Dehydrating** an instance writes it as a dehydrated module, and **hydrating** an artifact makes an instance from it (§3.1, §16.5).

The declarations of a module's root body define its logical **state**. A **row** is one structured value in a table, with an identity that the backend assigns (§4.2). A **view** is a typed read-only result derived from state. A **mutation**, declared with `mut`, is a typed sequential program proposing one atomic state transition. A **function** is an immutable or stable operation, admitted in every value position. A **Call** is a volatile or effectful operation, admitted only inside a transaction (§13.3).

A **transaction** runs one mutation call, one one-shot transaction (§11.1), one native transaction of the root client, one lifecycle operation, or one maintenance task over one snapshot, and proposes one transition. **Admission** validates and integrates the transition at one final serial position (§19). A successful state or composition change creates a **commit**, final once admitted.

The **root client** is the Rust program that embeds Liasse (§20). A **request** is one operation that a session sends (§11.1). An **actor** is the application row that authentication resolves for a session (§10.2). A **frontier** identifies the latest commit and temporal observation reflected by one live result (§11.2).

### 2.5 Syntax conventions

Module definitions are written in the native Liasse syntax that LANGUAGE.md specifies. LANGUAGE.md is the authority for every spelling, and this document for observable semantics. Definition examples use `liasse` code blocks, and each of them derives from the grammar of LANGUAGE Annex A (LANGUAGE §0.3). Values that cross a boundary — artifact records and every value on the wire — are CBOR (RFC 8949, Annex A.4); examples of them use CBOR diagnostic notation (RFC 8949 §8) in `cbor-diag` code blocks. Other logical notation, such as a sequence of frames, uses `text` code blocks.

---

## Part I — Package and application model

<a id="package-structure"></a>

## 3. Package structure

A `.liasse.zip` artifact is the one portable form of a module, and every backend reads and writes the same format. A **package** carries a module's current definition, every earlier version of it, and the migrations between consecutive versions, so that an instance can move up or down the whole chain from the package alone (§17). A **dehydrated module** is a package that also carries one instance's data, its history, the blob content they reach, and its mounted children, each dehydrated in turn.

### 3.1 The `.liasse.zip` format

A `.liasse.zip` artifact is a ZIP archive with the ZIP64 extensions (Annex C.3). Its entries are:

```text
*.liasse.zip
  <file>.liasse                             the current definition; its meta gives name, version, and language
  libraries/<file>.lib.liasse               the libraries it requires, the prelude included
  migrations/<version>/                     each earlier version: its <file>.liasse and its libraries/
  migrations/<from>-<to>.migration.liasse   each migration between consecutive versions
  data.cbor                                 dehydrated only: the instance, its selected point, its state, and its children
  history.cbor                              dehydrated only: the lineages, and the points with their deltas
  blobs/<hex SHA-512>                       dehydrated only: the content of one blob
  modules/<incarnation>.liasse.zip          dehydrated only: one mounted child, in the same format
```

- **Definitions.** The module file is the one entry at the archive root whose name ends in `.liasse` and in neither `.lib.liasse` nor `.migration.liasse`. `libraries/` holds every library that the current definition requires, directly or through another library, the prelude included, and a library is resolved by the `meta` it declares. Each earlier version lies under `migrations/<version>/` with the same layout: its own module file and its own `libraries/`. Each source file's `meta` gives its identity, whatever the file's name (§3.3).
- **The chain.** The current version and the versions under `migrations/` form the artifact's **migration chain**, in version order (LANGUAGE §13.2). Its lowest version is the chain's **floor**. Each two consecutive versions whose data structures differ are joined by the migration file `migrations/<from>-<to>.migration.liasse` (LANGUAGE §14.2).
- **Dehydrated entries.** A dehydrated module adds `data.cbor`, `history.cbor`, one `blobs/` entry for the content of each blob that its data or its carried history reaches, and one `modules/` entry for each child that its data or its carried history mounts (§16.5). Annex C.3 and Annex C.4 specify the records.

A reader ignores every other entry.

**Hydrating.** Hydrating a package runs the genesis of its current definition: its initializers and seed data (§8.1). Hydrating a dehydrated module loads the selected state of `data.cbor`, and of each child, recursively. `create` and `modules.mount` hydrate with fresh identities: every instance and row receives a new incarnation and identity, and the new instance's history starts with a genesis point that holds the loaded state. `restore` hydrates with the recorded identities, incarnations, and history, and `import` and `branch` bring the carried history into an existing instance (§8.2, §16.8, §16.11).

**Reading an artifact.** An operation that takes an artifact reads all of it, nested artifacts included, before it changes anything. It rejects the artifact, naming the entry, when:

- an entry name is unsafe or repeated (Annex C.3);
- a source file, a record, or a nested artifact does not parse;
- two entries disagree: a blob entry whose content does not have the SHA-512 digest that its name states, or a record that names a point, a child, or a version that the artifact must carry and does not (Annex C.4).

### 3.2 Source files

The extension of a source file gives its kind:

| Extension | Kind | Holds |
| --- | --- | --- |
| `.liasse` | module definition | the state, views, mutations, constraints, roles, authenticators, and mount points of one module (LANGUAGE §13) |
| `.lib.liasse` | library | stateless definitions: functions, templates, mixins, types, and interfaces, published under the library's `meta` and brought into a module through `requires` and `use` (LANGUAGE §3.2 and LANGUAGE §13.3) |
| `.migration.liasse` | migration | the structural delta and the data programs between two consecutive versions of one module (§17; syntax in LANGUAGE §14) |

### 3.3 Application, module, and instance identities

A definition's identity is its `name@version`, which its `meta` gives (LANGUAGE §13.2). The name identifies the line: an update moves an instance between versions of one name (§17.3). The version orders the definition in its migration chain.

A module instance is identified by its **mount point**. The root instance is identified by its application. A child instance is identified by the mount point that holds it: the identity of the row, struct, or module root that owns the mount point member, together with the member's declaration path (§12.3, Annex C.1). At most one module value occupies a mount point at a time, and two mount points holding modules of the same definition hold separate instances, with separate states, histories, and mounted descendants.

An instance lives from the transaction that mounts it, or for the root instance from the application's creation, until the transaction that unmounts it (§12.4). Each such lifetime has an **incarnation**: an opaque identity that the runtime assigns when the instance comes into existence and that distinguishes the successive instances that one mount point holds over time. A module mounted again at a mount point after an unmount is a new instance with a new incarnation and its own genesis; the earlier instance's state remains in history. History, export, and import name instances by incarnation (§16).

---

<a id="state-model"></a>

## 4. State model

The declarations of a module's root body define the logical shape, identity, and constraints of its state.

### 4.1 Fields and initializers

A field is a writable typed value of a row, a static struct, or the module root, and an initializer supplies its value when the member comes into existence and the writer supplies none (LANGUAGE §4.3 rules 1 to 6).

An initializer is evaluated once for each owner of its member, when the member comes into existence for that owner: when a module instance is created, for its root members and the members of its root structs; when a row is inserted, for the members of the row and of its structs; when an optional struct becomes present, for the members that the assigned record omits; and when a migration adds the member, for every owner that already exists (LANGUAGE §14.3 rule 6). The result is stored as ordinary writable state. An update that changes only an initializer leaves every existing value as it is; the changed initializer applies to owners created afterwards.

An initializer that reads state observes the transaction's intermediate state as it stood before the statement that creates its owner (§7.1):

```liasse
account_order: Int = count(/orders[.account == ^.account]) + 1
```

Every row that one statement inserts therefore observes the same state, and the rows of one bulk insertion become selectable together only after every row of that statement has resolved. The genesis load is the one exception (§8.1). An evaluation error in an initializer, such as a zero divisor or a position that requires one row and finds none, rejects the transition that creates the owner. A record that omits a required member is rejected before anything is written.

### 4.2 Tables

A table is the one kernel collection (LANGUAGE §4.3 rules 8 to 15).

**Identity.** Every row has an identity, assigned by the backend at insertion and kept through every change of its members until the row is deleted. Rows compare by identity, and a program reaches the identity only through its row: a selection over its members, a row of a relation, or a local bound to it; a client names a row through an occurrence token it received (§11.2). Display paths and diagnostics render the identity as opaque identity text (Annex C.1, C.2):

```text
/companies/k7Qx2m/offices/p4Rw9d/rooms/b2Nq8s
```

**Ownership.** Each occurrence of the owner has its own table: a root table exists once for each module instance, a table in a row body once for each row of the enclosing table, and a table in a struct body once for each occurrence of the struct. Each owner's rows are its own: `unique` declarations and insertion order are scoped to one owner. A row belongs to one owner for its whole life. It is inserted into an existing owner or together with a new one, it is removed with its owner, and it stays with that owner: moving data between owners is a removal and an insertion, and the inserted row has a new identity.

**Insertion order.** Every table has an insertion order. Each row takes a fixed position in it when it is inserted, unique among the rows of its owner and kept for the row's life, whatever later changes its members:

- the rows one statement inserts follow the order of the statement's operand, the statements of a transaction follow the order in which they run, and seed rows follow their array order;
- the rows of a transaction that completed before another began come before every row of the later one (§19.1);
- the rows of concurrent transactions are ordered arbitrarily but stably: once committed, their relative order is fixed.

**Changing `unique` values.** Writing a member that a `unique` names, by assignment, in a patch, or through a put or a replacement that matches the row, is an ordinary update: the row keeps its identity, its nested rows, its mounted modules, and its insertion position, and the `unique` is judged again on the complete result (§4.3).

**Nested rows.** A supplied nested relation is validated atomically with its owner's insertion, and a failure in a nested row rejects the owner's insertion.

### 4.3 Uniqueness

`unique(.a)` or `unique(.a, .b, …)` declares one **candidate key** over the rows of one owner. Its forms and the rows that take part in it are specified in LANGUAGE §6.3 rule 4, and its members' types are key-eligible (Annex A.5).

- **Equality.** Two values are equal when each component equals the corresponding one under its type's equality (Annex B).
- **Judgment.** A `unique` is judged on the complete result of every transaction: an intermediate state between its statements MAY hold one value in two rows. A value that two participating rows of one owner hold in the complete result rejects the transaction with the code `uniqueness`, whichever writes put it there: a strict insertion, even of a row equal to the other; a put; an assignment; a patch; or a replacement. The diagnostic names the table's path, the members, and the value.
- **Matching.** Where rows are matched by values, as a whole-table replacement matches its source occurrences, the `unique` declarations match them (§7.5), and an occurrence whose values of different `unique` declarations are held by different rows rejects the transaction.

---

<a id="expressions"></a>

## 5. Expressions

An expression computes a typed value from logical state and local bindings, with the syntax, roots, levels, and static meaning of LANGUAGE §8 to §10.

### 5.1 Contextual bindings

Three kinds of position define contextual bindings, and a binding exists only in a position that defines it. The authentication positions of an authenticator provide `context.credential`, `context.proof`, `context.session`, `context.actor`, and `context.auth` (§10.4). A request of an authenticated session to one of its roles provides `context.actor` and, when its authenticator selects a session row, `context.session` (§10.2). Migration programs and retype mappings provide `old` (§17). A read of `context.actor` or `context.session` where none is bound, such as in a mutation that a `public` mutation calls or in a mutation called through a mount point, faults and rejects its request (§9.2, §12.6); a read of either written lexically inside a `public` body is a static error (LANGUAGE §13.8 rule 5).

### 5.2 Selections

- The selection form `rows[c₁, c₂, …]`, relational σ over the occurrences of `rows` in its order, and its shorthands are specified in LANGUAGE §8.3 rule 4; a `filter` with the same conditions yields the same occurrences (LANGUAGE §9.3 rule 9).
- **One row.** A position that requires one row — a selection used as the receiver of an assignment, a patch, or a row-mutation call; a scalar conversion, such as a required value read through a selection; or the binding of `context.actor` or `context.session` — MUST receive exactly one occurrence. Zero or several occurrences reject that evaluation. A filtered receiver, such as `(.tasks |> filter(.done))`, instead applies a write to every row it selects, and to none when it selects none (§7.4).
- **Occurrences.** Repeated occurrences of one row, such as a row that a union reaches twice, remain distinct in ordinary view results and receive deterministic occurrence identities for live patches (Annex C.1). A write deduplicates the rows it targets by identity before applying one statement, keeping the first occurrence's order.

---

<a id="views"></a>

## 6. Views

A view is a typed, read-only result derived from logical state (LANGUAGE §7).

### 6.1 View identity

A view yielding rows inherits identity from its source row chain:

```text
.projects                      project identity
.projects.tasks                project identity + task identity
.companies.billing.invoices    company identity + mount point + invoice identity (§12.6)
```

Every stage but `group` preserves inherited identity, and `group` establishes its key as the identity of each output (LANGUAGE §9.3 rules 3 and 7).

---

<a id="mutations"></a>

## 7. Mutations and validation

A mutation is a typed sequential program that proposes one atomic state transition and MAY return a view of the committed result. A mutation is the only way a request changes state; the root client writes under the same types, `unique` declarations, and constraints (§20.1).

### 7.1 Mutation declarations

`mut name(inputs) { program }` declares a mutation, whose inputs, statements, and static rules are specified in LANGUAGE §10.

A transaction starts from one snapshot, runs its statements in sequence over its **intermediate state** — the snapshot together with every change that the statements before it made, the writes of the mutations they call included — and applies what they changed at its final apply (§19.1). Every read of a statement observes the intermediate state, so a later statement reads what an earlier one wrote, through a table, a selection, or a view. The last write to a place wins, and relative writes, insertions, and removals accumulate. The intermediate state is private to its transaction until the final apply. An intermediate state MAY violate a `unique` or a constraint: both are judged on the **complete result**, the state that the last statement leaves.

### 7.2 Receivers and internal calls

A mutation's receiver `.` is the row, table, struct, or module root whose body declares it (LANGUAGE §10.4). A receiver that selects one row runs the mutation on that row (§5.2). A filtered relation of rows, as in `(.tasks |> filter(.done)).archive()`, runs the mutation once for each row it selects, in its order; each run reads what the runs before it wrote, and a filtered relation that selects nothing runs it on none. A table-level mutation, called as `.tasks.add(title: "x")`, runs once for each table that its path reaches.

A called mutation runs inside the caller's transaction, at the calling statement, over the caller's intermediate state, and a failure anywhere rejects the whole transaction. A callee in the same module keeps the session's `context.actor` and `context.session`. A call through a mount point runs the mutation that its interface declares, as §12.6 states.

### 7.3 Local bindings and write values

`let name = expression` binds a local, and its expression is evaluated once. A local bound to rows holds those occurrences by identity: which rows it holds is fixed at the binding, and reading a member through it reads the row's value in the intermediate state where the read happens.

Some writes have a value, which a `let` or a `return` can use:

- `+=` on a table yields the rows it inserted, in source order: one row for a record or an element, and a relation for a relation or an array.
- A whole-table replacement yields the rows of the replacement relation, in source order.
- A put yields the rows it wrote: the matched rows in the selection's order, or the one row it inserted.
- `-=` on a table yields the removed rows as they stood immediately before the removal, in the operand's order after duplicate identities are removed by first occurrence.
- A mutation call yields the callee's response.

### 7.4 Writes

The write statements and patch blocks are specified in LANGUAGE §10.5. A write applies to the intermediate state when its statement runs: a relative write, such as `.balance -= amount`, applies to the value that the place holds then, and when the member or the operand is `none` it changes nothing. A patch block is one statement, and every right-hand side in it reads the intermediate state at the start of the patch. A filtered receiver, such as `(.tasks |> filter(.done)).archived = true`, assigns or patches every row it selects, once each and in its order, and none when it selects none.

### 7.5 Insert, replace, put, and delete

```liasse
.people += { name, email }
.people = .applicants |> map { .name, .email }
.people[.email == email] = { name }
.people -= .people[.disabled]
.tags -= "urgent"
```

**Insertion.** `rows += e` strictly inserts: it adds a new row for each occurrence of its operand, in source order, and an operand that is `none` inserts nothing. Every inserted row is a pending row with a new identity, so two equal inserted rows are two rows. Omitted members take their initializers when the statement runs (§4.1). When another row of the complete result holds one of an inserted row's `unique` values, the transaction is rejected with the code `uniqueness`, whether or not the two rows are equal and whether the other row is stored or inserted by the same transaction. A transaction that removes the row that holds a value, or changes that row's value, and inserts a row with that value commits, whatever the order of its statements. Inserting into an absent optional table makes it present.

**Put.** `rows[conditions] = r` replaces every row that the conditions match in the intermediate state, or inserts one row when they match none. The value sides are evaluated once, before the put, and a put with a value side that is `none` changes nothing. Each matched row becomes `r` together with the conditions' values: every member that `r` supplies takes the supplied value, every member it omits keeps its value, and the row keeps its identity. When no row matches, `r` is inserted with the conditions' values as `+=` inserts that record. The written rows take part in every `unique` in the complete result, so a put that gives several matched rows one value of a `unique` rejects the transaction.

**Replacement.** `table = e`, with `e` a relation or an array of records or elements, makes the table hold exactly the rows of `e`. A source occurrence that carries the identity of a row of the table matches that row, as when `e` reads the table itself; any other source occurrence matches the row of the intermediate state that holds its values of a `unique` of the table, and an occurrence whose values of different `unique` declarations are held by different rows rejects the transaction. A matched row keeps its identity and takes the supplied values, new values of its `unique` members included. An unmatched row of the table is removed. An unmatched source occurrence is inserted with a new identity. The resulting table is judged with the complete result, so a replacement that exchanges `unique` values among its rows is admitted when the result is valid. Assigning an array to a `Set` or a `Map` replaces its members in the same way.

**Removal.** `table -= e` removes the rows or elements that `e` denotes. An operand that is `none`, or a selection that matches no row, removes nothing. By rows, each row is removed once, duplicate identities being dropped by first occurrence; by element, every row whose element equals the operand is removed. Removing a pending row cancels its insertion. A removal deletes the row, every row nested in it, and every module mounted in them (§18.1).

### 7.6 Assertions and constraints

An assertion statement is judged once, when it runs (LANGUAGE §6.3 rule 6), whatever later writes of the transaction change what it read, and an assertion that reads state holds at the transaction's serial position (§19.1). What a session receives of a failed assertion or constraint is specified in §11.3.

### 7.7 Zero matches and unchanged calls

When the complete program produces no state change, the call returns `unchanged` and creates no commit: its complete result equals its snapshot, identities included. If the program ends in `return`, that response is still evaluated against the unchanged state and delivered with the `unchanged` status; the session's frontier stays where it was.

Writing a value equal to the stored one changes nothing, and neither does a put that finds the rows it names equal to its record, a removal of nothing, or an insertion that the same transaction removes. A transaction that changes state commits, whatever equal writes it also makes. A row removed and another inserted with equal values is a change, since the new row has a new identity.

### 7.8 Returning committed views

The response is evaluated from the final admitted state: the committed resulting state for `committed`, or the unchanged state for `unchanged`. A local that holds rows reads their values in that state. The response is call output, delivered once the transition has committed; a retry of the operation receives the final status without it (§11.3).

### 7.9 Calls and recorded values

`uuid()`, `random.bytes`, and every other Call produce their value once for an admitted request: a Call's result that enters committed state or another durable admission fact is recorded at admission and reused verbatim on replay. Each evaluation stands alone: `uuid()` yields a fresh, distinct value at every evaluation, so one initializer evaluated across the several rows of one insertion produces a distinct value per row. A transaction that is rejected or returns `unchanged` records nothing.

---

<a id="loading"></a>

## 8. Loading and bootstrapping

Bootstrapping activates a module together with the initial data required to make it usable.

### 8.1 Seed data

Seed data is the initializer of a table or a struct: the rows or the record that the member starts with.

```liasse
companies: Table {
    name: Text
    unique(.name)
    tags: Set(of: Text)
    offices: Table {
        name: Text
        unique(.name)
    }
} = [
    { name: "Acme SAS", tags: ["customer", "priority"], offices: [{ name: "Paris" }] }
]
```

- For a table without an element form, the array holds row records. Each record carries every required member except the members with an initializer and the mount points, and a table whose seed rows would need a required mount point is a static error (LANGUAGE §13.5 rule 1). An omitted member takes its initializer, a member that a `unique` covers included, such as the `id` of `identified()`. For a table with an element form, the array holds elements, or row records in their place. Nested tables and structs take the same forms at any depth, an omitted nested table starts empty, and records are closed.
- An initializer's rows are inserted as one strict insertion statement (§7.5): two rows with one value of a `unique` reject the definition, equal or not. The build rejects it when the values are build-time constants, and otherwise the load that inserts them is rejected. Every record is a new row, inserted in array order.
- Each value in a seed record is an expression. It MAY read what an initializer reads (§4.1), it is evaluated once, when its entry is inserted, and its result is stored as ordinary writable state.
- Seeding a view, a constant, a member that a component maintains, such as the versions of a keyring, or a mount point is a static error.

**When seed data applies.** Seed data applies exactly when its member comes into existence: at genesis, when the application is created or a module is mounted, for every member of the genesis definition; when a row is inserted, for the members of that row; and when a migration adds the member, for every owner that already exists (LANGUAGE §14.3 rule 6). Changing or removing seed data in a later release leaves existing state as it is.

**Genesis.** At genesis, the seed entries are inserted as one atomic load, through the same initializers, constraints, and `unique` declarations as a mutation's insertions. Every seeded row and supplied value is visible to every seed initializer, so an initializer such as `count(/items) + 1` observes all sibling seeded rows. Any failure rejects the whole load.

### 8.2 Root-client lifecycle operations

The root client alone controls the lifecycle of the root module, outside every role, through the operations of its library interface:

- `create(artifact)` hydrates a new application with fresh identities (§3.1). From a package, it makes a genesis: it instantiates the current definition and runs its initializers (§8.3). From a dehydrated module, it loads the artifact's selected state, recursively.
- `open(store)` reopens the active composition that a store records, after validating it and the definitions it requires.
- `update(artifact)` moves the root module along its migration chain to the artifact's version, upward or downward (§17.2, §17.3).
- `restore(artifact)` adopts a dehydrated module's recorded identities, incarnations, selected state, and retained history as they are, into an empty store (§16).
- `import(artifact, policy)` classifies a dehydrated module against the retained history and applies the movements its policy permits (§16.8).
- `export(selection)` dehydrates the selected boundary and points, recursively (§16.5, §16.7).
- `rollback`, `fast_forward`, `switch`, and `branch` are the history movements (§16.11).
- `transaction(…)` composes a native transaction over the whole module tree (§20.1).
- `bind` and `unbind` publish and withdraw context bindings, the contexts at which web clients open sessions (§10.1).

`create`, `update`, `restore`, and the activation of an `import` validate the complete prospective composition before activating it atomically: every definition with its language version, libraries, required components, and storage and key classes (§13.2, §14.1, §15.3), and a root definition that declares no `mount` (§12.5); the route and every migration along it, where an instance already exists (§17, §17.2); and the state of every mounted instance under its constraints, its `unique` declarations, and the interfaces between mount points and their children. A rejected operation leaves the prior active composition unchanged.

### 8.3 Bootstrap atomicity

The first successful `create` loads the definition and its genesis state — the seed data and initializers of the root definition — into fresh identities, records the genesis point of every instance's history, and activates the composition in one root-client transition. Genesis runs with native authority, outside every session, role, and actor.

### 8.4 Load outcomes

A lifecycle operation returns one of:

```text
committed   the validated definition, state, and composition became active
unchanged   the requested definition and state already apply
rejected    validation failed and the prior composition remains active
```

The root client receives structured diagnostics for rejected operations. A successful lifecycle operation is final once its resulting composition becomes active.

---

## Part II — Contexts, sessions, and clients

<a id="interfaces"></a>

## 9. Contexts, roles, and public surfaces

A program always runs in a context. A `public` body grants views and mutations to every session of a context, and a role grants them to the authenticated actors that its `members` view admits.

### 9.1 Contexts and grants

A **context** is the value that `.` denotes where a program runs:

- a mutation's context is its declaring body: the row, the table, the struct, or the module root whose body declares it (§7.2);
- a view's context is its declaring body in the same way;
- a session's context is where it mounts: the root of a module instance, or one row of a table whose row body declares a scoped role (§9.3, §10.1).

A **granted member** is a view or a mutation that a `public` or `role` body declares (LANGUAGE §13.7, LANGUAGE §13.8). A `public` or `role` body belongs to the context whose body encloses it.

```liasse
tasks: Table {
    identified()
    title: Text
    due: Date
    done = false
}

public {
    view open_tasks = .tasks |> filter(!.done) |> map { .id, .title }
    view due_before(day: Date) = .tasks |> filter(!.done, .due <= day) |> map { .id, .title, .due }
    mut add(title: Text, due: Date) { .tasks += { title, due } }
}
```

A session reads state only through a granted view, watches only granted views, and changes state only by calling a granted mutation. A granted view of rows grants reads only: a session reaches a row mutation through a granted mutation that selects the row, or through a role declared in that row's body (§9.3).

A granted member is addressed by its audience and its name, such as `public.open_tasks` or `member.my_tasks`. The audience is `public` or the name of a role, and the address is resolved in the session's context.

### 9.2 Public grants

A `public` body is the role that every session holds.

```liasse
public {
    view catalog = .products[.published] |> map { .id, .name, .price }
    mut request_invite(email: Text) { .invites += { email: string.lower(string.trim(email)) } }
}
```

In a public request, `context.actor` and `context.session` are unbound, whatever authentication its session holds (§10.5).

### 9.3 Roles, actors, and scoped roles

`role name(auth: a, members: v) { … }` declares an authenticated audience:

- `auth` names the authenticator, or the tuple of authenticators, that the role accepts, by path from the module root, as `/session` or `(/session, /api_key)` (§10.4);
- `members` is a view of actor rows, evaluated at every admission;
- the body declares the views and mutations that the role grants.

```liasse
role member(auth: /session, members: /accounts[.enabled]) {
    view my_tasks = .tasks[.owner == context.actor.id] |> map { .id, .title, .done }
    mut add_task(title: Text) { .tasks += { title, owner: context.actor.id } }
    mut complete_task(id: Uuid) { .tasks[.id == id, .owner == context.actor.id].complete() }
}
```

Actors are application rows: authentication resolves one application row as `context.actor`, and `members` decides whether that row holds the role at the request's admission position. `members` yields rows of the table from which every authenticator that the role accepts selects its actor; a role whose `members` type disagrees with an accepted authenticator's `actor` is a static error. The actor holds the role when its row identity occurs at least once in the resulting view; repeated occurrences grant the same authority as one.

Roles MAY be declared in a table's row body. Such a role is **scoped** to each row: `.` in its arguments and in its body is that row, and a session reaches it only by mounting at that row as its context (§10.1). A session mounted at a module root reaches the root's `public` body and roles; a session mounted at a row reaches the roles of that row's body.

```liasse
companies: Table {
    name: Text
    unique(.name)
    members: Table {
        account: Uuid
        admin = false
        unique(.account)
    }
    mut rename(name: Text) { .name = name }

    role admin(auth: /session, members: /accounts[.id in ^.members[.admin].account]) {
        view company = . |> map { .name }
        mut rename_company(name: Text) { .rename(name: name) }
    }
}
```

In the `members` view of `admin`, `.` inside the selection is each account and `^` the company row, so the role's members are the accounts that the company lists as administrators.

### 9.4 Denial

A request that the session is not authorized to have served — because the named audience or member is unknown in the session's context, is not granted, or names a private declaration; because the operation does not match the member's kind, such as a `call` of a view; or because the session's actor does not hold the targeted role — fails as an authorization denial (the `denied` class). The denial MUST NOT reveal whether a member of the named address exists, nor its kind, its input shape, or its key path: for a fixed session, the observable denial — its class and any diagnostic code — for an unknown name MUST be identical to that for a name that exists but is not granted to that session. Membership- or existence-specific diagnostics are permitted only toward a session that has already established authority over the target.

### 9.5 Recursive coverage

A scoped role MAY propagate its grants with `descend: d`, where `d` is a view over `.` that yields the included children:

```liasse
template Folder: Table {
    name: Text
    unique(.name)
    status = "open"
    editors: Set(of: Uuid)
    subfolders: Folder

    role editor(auth: /session, members: /accounts[.id in ^.editors.value], descend: .subfolders |> filter(.status != "closed")) {
        view folder = . |> map { .name, .status }
        mut rename(name: Text) { .name = name }
    }
}

folders: Folder
```

At each level:

1. `d` yields rows owned, directly or through nested tables, by the current row: strict descendants;
2. the role's body applies to each included child, with `.` bound to that child;
3. recursion descends only into included children; a child that `d` does not yield is pruned together with all its descendants.

Membership is evaluated at the scope row, the row at which the session mounted: every covered descendant is granted to the actors that the scope row's `members` admits.

A session reaches a covered descendant with a **key path**: one step per level, from the scope row down the relation that `descend` traverses. A step is the value of the first `unique` declared in that level's row body, in its CBOR form, as an array of the members' values in the `unique`'s order when it has several members. The empty path is the scope row itself. A step that does not parse against its level makes the request malformed. Admission evaluates the relation again along the whole path, and a path with a step that names no current row or a row that `d` does not include is denied.

Membership, descendant relations, and their filters are evaluated again at every admission, so a change of role membership or of coverage affects the requests ordered after it immediately, in serial order.

---

<a id="authentication"></a>

## 10. Authentication and sessions

An authenticator verifies a session's credential and resolves an application-defined actor, optionally through an application-defined session row, and the targeted role then authorizes that actor.

### 10.1 Sessions and context bindings

A **context binding** names one context and publishes it at an endpoint of a client binding (§11.1):

- the root of a module instance: the root module, or a module mounted at a mount point, whose root is a context exactly as the root module's is (§12.7);
- the rows of one table whose row body declares a role, together with one `unique` of that table. A web client that opens a session at such a context binding names one row by its values of that `unique`, in their CBOR form, as an array of the members' values when the `unique` has several members.

The context binding is where gating happens: the root client decides which contexts web clients can reach, at which endpoints, and under which host conditions, and a web client can mount only at a bound context. Binding and unbinding a context leave application state as it is, and unbinding a context closes the sessions mounted at it.

A **session** is one mount of a web client at one bound context. Opening it names the context and, optionally, an authentication: the name of an authenticator that the context's module declares, and a credential. The authentication is fixed for the session's life, so a web client that signs in, or presents another credential, opens another session. An unauthenticated session reaches only the `public` body of its context, which only a module root declares.

```cbor-diag
{"open": {"row": "acme", "auth": {"authenticator": "session", "credential": h'd28443a10126a058'}}}
```

Within the mount, the context's own authenticators and roles decide the session's grants: its `public` body, and each role of the context whose `auth` accepts the session's authenticator and whose `members` view holds the session's actor. Opening a session with an authentication checks that the context's module declares the named authenticator and authenticates the session with it, as §10.5 states short of the role test, and opening a session whose `row` names no current row, or whose authentication fails, is refused with the uniform denial (§9.4). Role membership is established per request, so a session whose actor holds no role of its context opens, and its requests to roles are denied.

The service identifies each session by a **session capability** that it mints, unguessable and opaque. Whoever holds the capability acts on that session. A session outlives the loss of the transport channel that carries it for as long as the service retains it, and the client then resumes its subscriptions (§11.2); a service MAY expire an idle session under host policy. The session is the unit of client coherence: the completion barrier, the order of frames, and the monotone frontiers hold per session (§11.2, §11.3). The service keeps the session's credential only for the session's life and never writes it into application state, history, an artifact, an operation record, a diagnostic, or a log.

### 10.2 Actors and session rows

Only a request of an authenticated session to one of its roles introduces `context.actor` and, when the session's authenticator selects one, `context.session`; calls within the module and the session's live views keep them (§7.2). Public requests, root-client native transactions, lifecycle and history operations, and engine maintenance run with both unbound.

### 10.3 Application-defined accounts, logins, and session rows

An **account** is an application row that MAY become the actor. A **login** identifies one verified external identity, such as an OIDC issuer and subject or a passkey credential. A **session row** is an application row representing continued authenticated access for one selected account, login, device, and lifetime.

```liasse
accounts: Table {
    identified()
    name: Text
    enabled = true
}

logins: Table {
    identified()
    kind: Text
    issuer: Text
    subject: Text
    data: Object
    unique(.kind, .issuer, .subject)
}

account_logins: Table {
    account: Uuid
    login: Uuid
    unique(.account, .login)
}

sessions: Table {
    identified()
    account: Uuid
    login: Uuid
    device: Text?
    created_at = now()
    expires_at: Timestamp
    revoked = false
    mut revoke() { .revoked = true }
    assert(.created_at < .expires_at, "A session expires after it starts")
}

view active_sessions = .sessions |> filter(!.revoked, .created_at <= now(), now() < .expires_at)
```

### 10.4 Authenticator declarations

An authenticator is a member of the module root whose definition is the `Authenticator` template that the service component publishes (LANGUAGE §13.8). A module MAY declare several:

```liasse
requires { service: "liasse.service@1", cose: "liasse.cose@1" }

session: service.Authenticator(
    credential: Bytes
    verify: cose.verify(/session_keys, context.credential)
    session: /active_sessions[.id == context.proof.session]
    actor: /accounts[.id == context.session.account]
) {
    assert(context.proof.auth == context.auth, "Token issued for another authenticator")
    assert(context.actor.enabled, "Account disabled")
}

api_key: service.Authenticator(
    credential: Bytes
    verify: cose.verify(/session_keys, context.credential)
    actor: /accounts[.id == context.proof.account]
) {
    assert(context.proof.auth == context.auth, "Token issued for another authenticator")
}
```

Bindings:

```text
context.credential   the credential the session presented
context.proof        the typed result of verify
context.session      the session row that the optional session attribute selects
context.actor        the row that the required actor attribute selects
context.auth         the name of the authenticator that the session selected
```

- `credential` is the type of the presented credential, decoded at the boundary from its CBOR form (Annex A.4); a credential that is absent or does not decode fails authentication.
- `session`, when declared, MUST resolve exactly one row, and `actor` MUST resolve exactly one row. Zero or several rows fail authentication.
- An authenticator is read-only, and its bindings exist for one request.

### 10.5 Selecting an authenticator

A role names every authenticator it accepts:

```liasse
role member(auth: /session, members: /accounts[.enabled]) {
    view my_tasks = .tasks[.owner == context.actor.id] |> map { .id, .title }
}

role automation(auth: (/session, /api_key), members: /accounts[.enabled]) {
    mut submit_job(kind: Text) { .jobs += { kind, submitted_by: context.actor.id } }
}
```

A session selects one authenticator when it opens (§10.1), and a request to a role is served only when the role's `auth` accepts that authenticator. The name is resolved in the module of the session's context and compared exactly, code point by code point.

A request to a role is authenticated in order: the credential is decoded against the authenticator's `credential` type, `verify` yields the proof, `session` resolves when the authenticator declares it, then `actor`, the authenticator's constraints are judged, and the actor MUST hold the role, with every step of a key path covered (§9.5). Any failure denies the request (§9.4). These evaluations read state, with `now()` the request's instant. They are established again at the request's serial position, where they are admission conditions of the request (§19.1), and for a live view at every outgoing frontier, with the frontier's temporal observation as the instant (§11.2). The reason of a denial is recorded for the operator only.

### 10.6 Signing in

A credential whose verification needs a Call — a passkey assertion, an OIDC response, an API-key exchange, a password hash — is verified in a sign-in mutation, an ordinary public mutation: it makes the verifier Call, maps the verified identity to a login and an account, records a session row, and mints a native token (§14.8) that an authenticator's `verify` accepts on later requests:

```liasse
requires { service: "liasse.service@1", cose: "liasse.cose@1", webauthn: "liasse.webauthn@1" }

public {
    mut passkey_login(response: Object, account: Uuid, device: Text?) {
        let identity = webauthn.verify(response)
        let login = /logins[.kind == "passkey", .issuer == identity.rp, .subject == identity.credential]
        assert(has(login), "Unknown passkey")
        assert(has(/account_logins[.account == account, .login == login.id]), "This passkey is not linked to that account")
        let row = /sessions += { account, login: login.id, device, expires_at: now() + time.duration("P30D") }
        let token = cose.sign(/session_keys, {
            auth: "session", session: row.id, issued_at: now(), exp: row.expires_at
        })
        return { auth: "session", token, expires_at: row.expires_at }
    }
}
```

The client opens a session with the `session` authenticator and the returned token (§10.1).

### 10.7 SSO and account selection

A sign-on verifier resolves a provider identity, and the application maps that identity to one or more local accounts.

```liasse
public {
    mut oidc_callback(response: Object) {
        let identity = oidc.verify(response)
        let login = /logins[.kind == "oidc", .issuer == identity.issuer, .subject == identity.subject]
        assert(has(login), "Unknown identity")
        let links = /account_logins[.login == login.id]
        assert(count(links) > 0, "No account is linked")
        let ticket = cose.sign(/session_keys, {
            purpose: "account-selection", login: login.id, exp: now() + time.duration("PT5M")
        })
        return { ticket, accounts: /accounts[.id in links.account] |> map { .id, .name } }
    }
}
```

The callback returns a short-lived **selection ticket**, signed with a keyring, whose claims name the verified login and a purpose that every authenticator rejects. A follow-up public mutation verifies the ticket, checks its purpose and its expiry, and records a session row, then signs the token, only for an account linked to the login that the ticket names.

### 10.8 Revocation and expiry

A session row is revoked by a write that its authenticator reads, such as the `revoked` mark that `active_sessions` filters out:

```liasse
role member(auth: /session, members: /accounts[.enabled]) {
    mut sign_out() { /sessions[.id == context.session.id].revoke() }
}
```

- Revocation affects the requests ordered after the revoking commit: every later request of a session whose token names that session row fails authentication and is denied. A request already committed remains final, and a request ordered before the revoking commit is authenticated at its own position and MAY commit. A request that revokes its own session row is admitted, since it authenticated at its own position, before its write.
- Every live view of a session whose authentication fails closes with `unauthorized` at the first outgoing frontier at or after the revoking commit (§11.2); on the session that made the revoking call, that happens before the call's `committed` is returned.
- A session row is active over the interval that it stores, `[created_at, expires_at)`, and the authenticator selects it through the time-dependent view `active_sessions`. A request whose instant is at or after the end is denied, and a live view of the session closes with `unauthorized` at the temporal frontier that observes the crossing, by time alone. An expired session row stays in state.
- Authority also ends, with the same effects, when the actor leaves the role's `members`, when a constraint of the authenticator stops holding, such as for a disabled account, and when the token's key version stops being accepted (§14.3).

### 10.9 Several sessions

A frontend MAY hold several sessions at once, each one mount with its own authentication:

```text
the public body of the root module           no authentication
company acme, as a personal account          session authenticator
company globex, as a work account            session authenticator
company acme, as an automation               api_key authenticator
```

Each request belongs to exactly one session and takes its context and its authentication from that session alone. The completion barrier of a call covers the calling session's subscriptions only, and two sessions of one frontend are related only through the order of observed commits (§11.3).

---

<a id="clients"></a>

## 11. Clients and live views

A web client uses an application through sessions. It calls granted mutations, alone or as one-shot transactions, and MAY keep granted views synchronized.

### 11.1 Session operations and one-shot transactions

**Client bindings.** A **client binding** carries sessions over a transport: HTTP requests with one Server-Sent Events stream per session, or QUIC with WebTransport, one WebTransport session per Liasse session and one stream per subscription. One transport channel MAY carry several sessions. Every message that a client binding carries — a request, an outcome, a frame, a manifest, or a status — is one CBOR data item (Annex A.4); a transport that carries only text, such as Server-Sent Events, carries the item's bytes in unpadded base64url. A client binding MAY add the members of its transport, such as the session capability, and MUST NOT add an operation that reads or writes application state. A message that names no operation, or that does not parse as one, is a transport fault.

**Operations.** A session reaches application state through five operations:

| Operation | Target | Effect on state | Result |
| --- | --- | --- | --- |
| `manifest` | the session | read-only | the members granted to the session |
| `view` | a granted view | read-only | a subscription: `init`, then advances, until `close` (§11.2) |
| `call` | a granted mutation, or an ordered list of them | proposes one transition | an outcome (§11.3) |
| `fetch` | a blob that a granted view shows | read-only on application data | a fetch plan (§15.4) |
| `operation` | an earlier call | read-only | its retained status (§11.3) |

- `unsubscribe` closes a subscription that the session opened, at any time. It is part of the `view` operation and leaves state as it is.
- A **one-shot read** is a `view` whose subscription the client closes after its `init`. A client binding MAY offer a one-shot read as a single exchange; it then MUST parse, authorize, and project the read exactly as a `view` closed after its `init`, and report the same refusals.

**Requests.** Each request is a CBOR map whose members are fixed. The `call` operation has two request forms, a single call and a transaction:

| Request | Members |
| --- | --- |
| manifest | `"manifest"`: an empty map |
| view | `"view"`: the address; `"sub"`: the subscription identifier, a text that the client chooses; `"args"`: the argument object; `"descend"` (optional): the key path (§9.5); `"from"` (optional): the last frontier that the client applied, to resume (§11.2) |
| unsubscribe | `"unsubscribe"`: the subscription identifier |
| call | `"call"`: the address; `"args"`: the argument object; `"descend"` (optional): the key path; `"op"` (optional): the operation identifier, a text that the client chooses (§11.3) |
| transaction | `"transaction"`: a non-empty array of calls, each a map of `"call"`, `"args"`, and optionally `"descend"`; `"op"` (optional): the operation identifier |
| fetch | `"fetch"`: the address; `"args"`: the argument object; `"descend"` (optional): the key path; `"occurrence"` (optional): the occurrence token of the row that holds the blob, for a view that yields rows; `"path"`: the array of member names from that value, or from the view's one value, down to the `Blob` |
| operation | `"operation"`: the operation identifier; `"target"`: the address of the call, or the array of the addresses of a transaction's calls |

The member set of each request is closed: an unknown member, a missing required member, or a member of the wrong type makes the request malformed. Opening a session is an exchange of the client binding (§10.1): `{"open": {…}}` with the optional members `"row"`, the row's values of the `unique` that the context binding names (§10.1), and `"auth"`, a map of `"authenticator"`, the authenticator's name, and `"credential"`, the credential in its CBOR form; its reply carries the session capability, and it reads application state only to authenticate. An address is a text of two segments, the audience and the member name, separated by `.`, and each segment is compared with the declared name by exact code points. A text that is not of the address form is malformed and is rejected before anything is resolved, revealing nothing about the application. Tokens that the service mints — session capabilities, frontiers, commits, and occurrence tokens — are CBOR byte strings; identifiers that the client chooses are text strings, compared by exact code points.

**Request pipeline.** The service processes a `call` in this order. The first step that fails decides the outcome and ends the request, and state stays as it was:

1. **Resolve** the target: the audience and the member in the session's context, and the key path.
2. **Authenticate**: for a role audience, run the steps of §10.5; for `public`, leave `context.actor` unbound.
3. **Parse** the argument object against the member's declared inputs.
4. **Judge** the constraints of the inputs' types (LANGUAGE §10.3 rule 10).
5. **Receive** the content of every `Blob` argument: stream, stage, and verify it against the argument's descriptor (§15.2).
6. **Admit** the request at its serial position, concurrently with other requests: establish again the session row's validity, the actor's role membership, and the coverage of the key path, and run the transaction (§19).
7. **Commit** the transition atomically, or end as `unchanged` or `rejected`.
8. **Advance** every still-authorized subscription of the calling session through the commit, and only then return `committed` (§11.3).

- Steps 1 and 2 decide together, and every failure of either is the uniform denial (§9.4).
- Step 6 is authoritative. A request that passed step 2 is still denied at step 6 when a commit ordered before it removed the actor's membership, invalidated its session row, or pruned its key path.
- A `call` that carries an operation identifier is matched against the records of its scope once step 3 has decoded its arguments (§11.3).
- The other operations run a prefix of these steps: `manifest` runs step 2 for each role it lists; `view` runs steps 1 to 4, evaluates the view at a frontier, and opens the subscription; `fetch` runs steps 1 to 4, checks that the blob is visible, and issues the plan; `operation` runs steps 1 and 2 for the target's audience and looks up the record.

**Argument objects.** A `view`, `call`, or `fetch` request carries the **argument object** of its member: a CBOR map of the member's named inputs or parameters.

- An argument object is closed: it MUST contain only names that are declared inputs of the targeted mutation or parameters of the targeted view. A member whose name is not declared — a name beginning with `$` included — makes the request malformed. A struct value nested in an argument is closed in the same way (LANGUAGE §5.4 rule 4).
- A missing required input makes the request malformed, and an omitted optional input is `none`. An omitted input with a default takes its default, evaluated once for the request: at admission for a `call`, and when the subscription opens for a `view`.
- Every value is a CBOR item that decodes to a value of its declared type (Annex A.4), and an item that decodes to no such value makes the request malformed. A repeated element of a received `Set`, and a repeated key of a received `Map`, make the request malformed. A CBOR `null` stands only where Annex A.1 places `none`, such as an absent place of a tuple: an omitted optional input is left out of the argument object, and a `null` anywhere else makes the request malformed.
- The content of each `Blob` argument travels beside its `call` through the client binding, and the argument object carries the blob's descriptor; step 5 of the pipeline receives the content.
- Diagnostics omit argument values.

**One-shot transactions.** A `call` carries one granted mutation call, or a **transaction**: an ordered list of granted mutation calls from the same session.

```cbor-diag
{"transaction": [
    {"call": "public.add",      "args": {"title": "A"}},
    {"call": "public.add",      "args": {"title": "B"}},
    {"call": "public.complete", "args": {"id": 37(h'9b2e4c1a5d3f4e8ba6c70f1d2e3a4b5c')}}
]}
```

It commits or rejects as a whole, with one result per call:

- **Authorization.** Every call of the list is resolved, authenticated, and parsed (steps 1 to 4) before any runs, and each is authorized as if it were made alone. A denial of any call denies the whole request with the uniform denial. A malformed call, or a failed input-type constraint, rejects the whole request, and the rejection names the call's index.
- **Execution.** The calls run in list order as one transaction over one snapshot. Each call reads the intermediate state that the calls before it left (§7.1). The complete result is judged once and commits atomically, or not at all. A failure anywhere rejects the whole list, and the rejection names the index of the failing call.
- **Results.** The outcome carries one result per call, in list order: the value of that call's `return`, evaluated from the complete result as a single call's response is (§7.8), or `null` for a mutation that declares no response.
- **Outcome.** A transaction ends as `committed` with one commit and one frontier, as `unchanged` when its complete result changes nothing, as `rejected`, or as `denied`.
- **One round trip.** A transaction is sent whole and runs within its one request. A committed transaction is one commit in the history of each instance it changed, one advance of every live view, one completion barrier, and one operation record.
- An operation identifier on a transaction names the whole list, and the calls inside the transaction share it.

**Manifest.** A `manifest` request lists the members granted to the session: the members of the `public` body of its context, and the members of each role of its context that accepts the session's authenticator and whose `members` holds the session's actor at the request's instant.

- For each member, the manifest gives its address; its kind, view or mutation; each input or parameter, with its name, its type, and whether it is required, optional, or defaulted; the result of a view, which says whether it yields rows or one value, or the response type of a mutation when it declares one; and, for a role with `descend`, the form of a key path step. Types are described by the type descriptors of Annex A.2.
- The manifest describes the active definition and the current grants when it is read, and it changes when either changes: after an update, and when a grant stops holding.

**Opaque tokens.** A value that one side mints for the other is an opaque token. The side that receives it stores it, compares it only for equality, and sends it back unchanged; only the side that mints a token builds it, and its spelling is opaque.

| Token | Chosen by | Names | Valid on | Grants |
| --- | --- | --- | --- | --- |
| session capability | service | one session | the service that minted it | acting on that session: its subscriptions and its completion barrier |
| subscription identifier | client | one open subscription | its session | nothing |
| frontier | service | a position of a subscription stream | its session | nothing |
| commit | service | the commit of one call | the application: every outcome and status that reports the call carries the same one | nothing |
| occurrence token | service | one occurrence of a view's result | the session that received it | nothing |
| session token | the application, in a mutation's response | one session row | the application, in any session | authentication, through its authenticator (§10.4) |
| operation identifier | client | one call or transaction | its scope (§11.3) | for a public call, reading its status |

A token that the service mints refers to what it names through the service's own records, and the service draws the random part of every token from a cryptographically secure source. A frontier, a commit, or an occurrence token that the service did not mint for that use on that session is refused at the boundary, before admission, and the refusal reveals nothing about the token: a frontier is treated as a released range (§11.2), and an occurrence token makes the request malformed.

**Failure classes.** A request that does not succeed ends in exactly one of these classes:

| Class | Meaning | Operations |
| --- | --- | --- |
| `denied` | authorization refused the request: an address that names no member of the session's context, a member not granted to the session, a private declaration, an operation that does not match the member's kind, a failed authentication, a role that the actor does not hold, a key path step that is not covered, or a blob that the named view does not show | every operation except `unsubscribe` |
| `rejected` | the request was addressed to a target that the session may use, and was refused: it is malformed, an input constraint failed, an admission condition or a constraint failed, the evaluation failed, or the operation identifier conflicts | `view`, `call`, `fetch`, `operation` |
| `failed` | a read could not be served in the current state: every source failed to deliver a blob's content (`unavailable`) | `fetch` |
| transport fault | the client binding could not process the message: it does not parse, it is too large, it presents a session capability that the service cannot open, or the outcome of its commit could not be established | every operation |

- A transport fault leaves the outcome of a `call` unknown to the client, which learns it with `operation` or by retrying with the same operation identifier (§11.3); so does a `call` whose reply is lost. The transition MAY have committed.

**What crosses the wire.** Toward the client, the service sends only authorized projections of granted results, outcomes with their codes and messages, and opaque tokens: exactly the members and rows that each projection exposes. Internal identities — a row identity, an incarnation, a backend snapshot or transaction identifier, or a session row's internal identifier — stay on the service. The module's source, such as a view's definition, a default expression, or a constraint's condition, stays on the service outside development mode; the message of an `assert` is text that the module authored, and a rejection MAY carry it. Toward the service, a session sends names, typed values, and tokens only.

### 11.2 Live frontiers and resumable views

A live subscription begins with a complete result and a frontier, then receives ordered advances:

```text
init(frontier, rows)          the complete result, each occurrence with its token
init(frontier, value)         the one value of a view that yields one value
patch(frontier, operations)   operations that turn the previous result into the result at frontier
value(frontier, value)        the new value of a view that yields one value
close(frontier, reason)       the end of the subscription
```

Each frame is a CBOR map:

| Frame | Encoding |
| --- | --- |
| `init` of rows | `{"init": {"sub": id, "frontier": f, "rows": [[token, value], …]}}` |
| `init` of one value | `{"init": {"sub": id, "frontier": f, "value": value}}`, with `"value"` omitted when the value is `none` |
| `patch` | `{"patch": {"sub": id, "frontier": f, "ops": [operation, …]}}` |
| `value` | `{"value": {"sub": id, "frontier": f, "value": value}}`, with `"value"` omitted when the value is `none` |
| `close` | `{"close": {"sub": id, "frontier": f, "reason": reason}}` |

A frontier is an opaque, totally ordered position within one subscription stream. It covers committed changes and temporal observations: a view that reads `now()` advances when the clock crosses a bound that changes its result, by time alone (§19.3). Frontiers of different subscriptions are independent. The first frontier of a subscription is at or after every commit that its session reported as `committed`, and every frontier at which it reported `unchanged`, before the service received the `view` request. Resuming from a retained frontier yields the later authorized frames of that stream, or a fresh `init` when the runtime has released the required range.

The runtime re-evaluates authentication, session row validity, role membership, key-path coverage, the availability of the view, and its projection at every outgoing frontier. When the current state removes the subscription's authority or its view, the frame that the runtime emits at that frontier is `close` with the reason `unauthorized`, carrying the frontier and the reason alone; the client releases the result it holds for that subscription. A change of authority takes effect at the frontier where the commit or the temporal observation that causes it becomes visible, and it closes only the subscriptions that depend on it.

Each occurrence of a result of rows has an opaque occurrence identity, which the wire carries as an occurrence token. Within one stream, from an `init` until the next `init` or the `close`, each occurrence keeps one token, and each token names that one occurrence, even after the occurrence left the result and came back. Across the streams of one session, each token names one occurrence identity (Annex C.1), so the token that a `fetch` presents names one occurrence.

Patch operations are applied in listed order, each to the result that the operations before it left:

```text
["insert", at, token, value]   insert the occurrence with its value at the zero-based position at
["remove", token]              remove the occurrence
["move", token, to]            move the occurrence to the zero-based position to of the result without it
["update", token, value]       replace the occurrence's value, keeping its identity and position
```

After applying every operation, the client's result MUST equal the authorized declared view at the new frontier: the same occurrences, with the same values, in the same order, and, inside each value, the same nested relations, which the client reconstructs as nested structures. Values in operations are complete: an `update` carries the occurrence's whole new value. An occurrence that is in the result at both frontiers keeps its token, and a patch changes it only through `update` and `move`: a change of the members that a `unique` names is an `update`, a `move`, or both. A frontier-only patch has an empty operation sequence. A patch MAY cover several commits and temporal observations at once, with any operation sequence that yields the new result. A patch that names an occurrence outside the result, inserts one already in it, or names a position out of range is inconsistent with the client's structure: the client discards the subscription's result and opens the subscription again.

A view whose result is one value — a scalar, a record, or an optional value — begins with `init(frontier, value)`, and receives `value(frontier, value)` at each later frontier where the value changed. A value of `none` is absent.

Every frame of one subscription is delivered, in order. A service that cannot deliver a frame, for example because the client reads too slowly, MUST NOT drop it silently: it closes the subscription with `close(frontier, "server-closed")`, and the client sends the `view` request again with its last applied frontier, which the service answers with the later frames or a fresh `init`. Frames of different subscriptions of one session MAY interleave.

A `view` request fixes its subscription for its whole life: its address, its argument object, its key path, and its session. A `view` request that names the identifier of an open subscription takes its place, closing it with the reason `replaced`, unless it resumes that subscription: a resume presents the last frontier applied, in a `view` request with the same identifier, address, argument object, and key path. The service authorizes a resumed subscription again before any frame flows. A frontier that was not issued for this stream is treated as a released range and answered with a fresh `init` of this stream. After a fresh `init`, the client discards the whole result it held and keeps the new one, whose tokens MAY differ.

A subscription ends with `close(frontier, reason)`:

| Reason | When |
| --- | --- |
| `unsubscribed` | the client closed the subscription |
| `unauthorized` | the subscription lost its authority or its view; both causes give this one reason |
| `replaced` | the client opened another subscription under the same identifier |
| `server-closed` | the service ended the subscription for another reason: the view's evaluation failed at a later frontier, the service could not deliver a frame, or the host closed the session |

An update keeps the subscriptions to the views that remain: at the update's frontier they are evaluated under the new definition and patched, and their occurrences keep their tokens; a subscription to a view that the new definition removes, or stops granting to its session, closes with `unauthorized`.

### 11.3 Completion, deduplication, and status

Receiving `committed` proves that the transition is final and that the authorized live results of the same session have advanced through that commit. Receiving `unchanged` proves evaluation at the returned frontier.

Outcomes are CBOR maps:

| Outcome | Encoding |
| --- | --- |
| committed | `{"committed": {"frontier": f, "commit": c, "result": value}}`; `"result"` is present when the mutation declares a response, and a transaction carries `"results": [value, …]` in its place |
| unchanged | `{"unchanged": {"frontier": f, "result": value}}`, with results as for `committed` |
| rejected | `{"rejected": {"code": text, "message": text, "call": index}}`; `"call"`, the zero-based index of the failing call, is present for a transaction |
| denied | `{"denied": {"code": text, "message": text}}` |
| failed | `{"failed": {"code": text}}` |

The code of a rejection is one of the rejection codes of §20.3; the code of a failed read is `unavailable`. A deployment in development mode MAY add a `"debug"` member, the diagnostic's path and trace (§20.3), to a rejection and to a failed read; a denial keeps its one fixed form. Every denial has the code `denied` and one fixed message, whatever its reason (§9.4).

**Completion barrier.** For a call whose outcome is `committed`, the service MUST, in this order: commit the transition durably; make it visible to every read at that commit or later; establish again the authorization of every outgoing result of the calling session; advance every still-authorized subscription of the calling session through the commit, closing the ones that lost their authority; and deliver the `committed` outcome. A subscription that the commit does not affect MAY advance with a frontier-only patch. When the stream of a session and the replies to its requests travel on separate channels, the service sends every frame through the commit on the stream before it sends the reply. The barrier covers the calling session only: every still-authorized subscription of every other session advances through each commit, in serial order, eventually. Before returning `unchanged`, the service advances every still-authorized subscription of the calling session to at least the returned frontier.

A connection loss before admission MAY cancel the request. A connection loss while admission or response delivery is in progress leaves the outcome unknown to that client: the request may have committed, and a committed transition remains final. Once admitted, a call proceeds to its terminal outcome whether or not its channel survives.

**Operation identifiers.** A call or a transaction MAY carry an operation identifier: a high-entropy identifier, which a client SHOULD draw from a cryptographically secure generator. Its scope is the application, the bound context at which the session is mounted, the target — the address of the call, or the ordered addresses of a transaction's calls, together with their key paths — the session's authenticator when the target's audience is a role, and the identifier. Reusing the same scoped identifier with an equivalent request provides at-most-once execution. Reusing it with a request that is not equivalent rejects the call with the code `conflict`, before admission; the original operation and its record stay as they are. Each submission of a call that omits the identifier is a new operation.

Two submissions of one scoped operation identifier are **equivalent** when their decoded argument objects (§11.1), in call order for a transaction, their key paths, and, for a role audience, the actors that they authenticated as are equal. An equivalent retry is answered with the retained status of the first submission, once that submission has ended, with the same commit and diagnostic; the mutation runs once, and the response travels with the first submission alone.

A retained operation record reports one of:

```text
pending
committed { frontier, commit }
unchanged { frontier }
rejected { code, message }
unknown
```

encoded as `{"pending": {}}`, `{"committed": {"frontier": f, "commit": c}}`, `{"unchanged": {"frontier": f}}`, `{"rejected": {"code": text, "message": text}}`, and `{"unknown": {}}`. A record exists for every call with an identifier whose argument object parsed and that passed authorization, from the moment the service accepts it. The runtime MAY expire records according to host policy, after which the status is `unknown` and a submission with the identifier is a new operation. For a public call, the high-entropy operation identifier also acts as the capability required to query its record; for a role call, the service answers only a query whose session selects the same authenticator and authenticates the same actor, and the status is `unknown` to any other.

**Uncertain commits.** An input or output failure during a commit leaves its outcome unknown, and the service reports that outcome as `unknown`. The service MUST NOT run the call again while its outcome is unknown.

**Client bindings.** A client binding conforms when it preserves these guarantees for its caller. It MUST NOT present a call as `committed` before the structures it reconstructs for the session's live views have applied the stream through the returned frontier, and MUST NOT present `unchanged` before they reached its frontier. It MUST NOT present an unknown outcome as a failure or as permission to submit the call again as a new operation. It retries a call only with the same identifier and an equivalent request, and MUST NOT retry on its own a call that carries no identifier.

---

## Part III — Composition and advanced features

<a id="modules"></a>

## 12. Modules

A module is a unit of state, views, mutations, roles, and authenticators. A mounted module is a value of type `Module(interface: I)`, held by exactly one mount point of its parent: the parent reaches it only through its interface, and it reaches its parent only through the context that the mount point provides.

### 12.1 Module packages

A module is defined by one module definition and packaged as a `.liasse.zip` package artifact (§3.1). A module has one form: the same definition serves as the root module of an application or as a child mounted at a mount point. Two mounts of one package artifact are two instances, with separate states, histories, and mounted descendants (§3.3).

### 12.2 Interfaces

An interface is a contract of view and mutation signatures, which a module satisfies structurally through its root members (LANGUAGE §13.4):

```liasse
interface Ledger {
    view entries: Table { number: Text; total: Decimal }
    view balance: Decimal
    mut charge(amount: Decimal)
    mut refund(number: Text): Decimal
}
```

Satisfaction is checked at mount (§12.4), at every update of the child or of the parent, and on the result of every transaction that updates the parent or the child. A failing check rejects the transition, and the diagnostic names the mount point, the interface, and the member.

### 12.3 Mount points

`Module(interface: I)` is a kernel template. A member of that type is a **mount point**: a place that holds at most one module value satisfying `I`, where `I` is a named interface or an inline interface body (LANGUAGE §13.5).

```liasse
workspace: {
    analytics: Module(interface: { view events: Table { at: Timestamp; kind: Text } })?
}
```

A mount point is declared at a module root, in a struct body, or in a table's row body, where it declares one mount point per row, so a table with a `Module` member holds one independent module instance per row. An optional mount point is empty until a module is mounted, and `has(.billing)` tests whether it holds one. A required mount point MUST hold a module in every committed state in which its owner exists, and the requirement is judged on the complete result: the transaction that inserts a row with a required mount point mounts its module before it ends, or it is rejected. A mount point's display path is its owner's path followed by the member's name, such as `/companies/k7Qx2m/billing`, and the mount point identifies the instance it holds (§3.3, Annex C.2).

### 12.4 Mounting, updating, and unmounting

Mounting, updating, and unmounting are Calls of the standard namespace `modules` and writes of the mount point. They are admitted only inside a transaction: in a mutation program, and in a native transaction of the root client (§20.1).

```liasse
mut enable_billing(artifact: Blob) { .billing = modules.mount(artifact) }
mut upgrade_billing(artifact: Blob) { modules.update(.billing, artifact) }
mut disable_billing() { .billing = none }
```

**Mount.** `modules.mount(artifact: Blob)` reads the content of the blob, a package or a dehydrated module (§3.1), through the content read of §15.4, and returns a module value that the statement assigns to a mount point. The artifact's current definition becomes the child's active definition, together with its migration chain. Within the calling transaction, mounting validates the child as `create` does (§8.2), checks that it satisfies the mount point's interface (§12.2) and that the provided context satisfies its `mount` declaration (§12.5), and hydrates it with fresh identities and a new incarnation (§3.1): from a package, it runs the child's genesis (§8.1); from a dehydrated module, it loads the child's selected state and the children that state mounts, recursively. Any failure, or a constraint or `unique` of the child or the parent that fails on the complete result, rejects the whole transaction. The child's first point is the genesis of its own history, and it shares the transaction's identity with the parent's commit (§16.1). A mount point that already holds a module rejects a mount, and the transaction with it; a program that assigns `none` to it first MAY then mount another in the same transaction.

**Update.** `modules.update(.m, artifact: Blob)` moves the module mounted at the mount point `.m` along its migration chain to the version of the artifact's current definition, upward or downward, applying the migrations of the route in order (§17.2, §17.3). The artifact MUST carry the same `meta.name` as the child's active definition, and the route MUST exist. An update of an empty mount point rejects the transaction. A transaction that updates a parent MAY also mount, update, and unmount its children, and the combined result is judged once (§17.3).

**Unmount.** Removing the value unmounts the module: assigning `none` to the mount point, removing the row that owns the mount point, or removing a row or an instance that owns that row (§18.2).

### 12.5 Provided context

A child declares the context it needs with a `mount` declaration, and the mount point that holds it supplies that context with `provide` (LANGUAGE §13.6):

```liasse
billing: Module(interface: Billing)? {
    provide({ view name = ^.name; view plan = ^.plan })
}
```

```liasse
mount company: Company
view header = #company.name + " invoices"
```

The provided views are evaluated in the parent at the level of the mount point's declaration, where `^` is the mount point's owner. They MUST satisfy the child's `mount` declaration at mount, at every update of the parent or of the child, and on the result of every transaction that updates either one, and a failure rejects the transition. A child's view, constraint, or program reads `#name` as a live read of the parent's state at its own snapshot and instant, and a live view of the child that reads it advances when the parent state it reads changes. A child's constraint that reads its provided context is judged again when that state changes (LANGUAGE §6.3), so a parent's write that breaks it is rejected.

### 12.6 Reading into and mutating a module

`.m.v` reads the view `v`, or the data read as a view, that the mount point's interface declares, inside the child that `.m` holds; traversal through a table of mount points flattens with identity (§6.1, LANGUAGE §8.3 rule 5). Through an empty optional mount point, a relation yields an empty result and a one-value view yields `none`.

`.m.f(args)` runs the child's mutation `f` that the interface declares, inside the caller's transaction and atomically with it: the writes of both modules are one transition, and a failure in either rejects both. The child's mutation runs with the child's root as its context, reads the child's intermediate state, and changes only the child's state. `context.actor` and `context.session` are unbound in it, and a read of either faults (§5.1). Its response is projected to the result that the interface signature declares. A call through a mount point that a selection names requires that mount point to hold a module, and rejects the transaction otherwise, as a receiver that selects no row does. A call through a filtered relation, such as `(.companies |> filter(.plan == "pro")).billing.charge(amount: 10.0)`, runs the mutation once for each module that the relation reaches. A child's state changes only through the mutations of its interface (LANGUAGE §10.5 rule 5).

### 12.7 Sessions and modules

A module behaves identically whether it is the root or a child:

- its own `public` body, roles, and authenticators apply at its own contexts. A session mounted at a child's root, or at a row of a table inside the child, is authenticated by the child's authenticators and authorized by the child's roles, and a child's authenticator MAY read what its mount point provides, such as the parent's active session rows (§12.5);
- web clients reach a child through a context binding that the root client makes for the child's context, naming the child by its mount point (§10.1);
- a parent grants what a child holds only by granting its own views and mutations, which read through the mount point or call the child's interface (§9.1);
- the root client's native access reaches every instance, the root and every child, under the same rules (§20.1).

---

<a id="host-namespaces"></a>

## 13. Host namespaces

A host namespace is a typed contract of functions, Calls, types, and templates, implemented in Rust by a component and registered in the Liasse context.

### 13.1 Core functions and namespaces

The standard core contains:

```text
language     arithmetic, logic, selections, views, aggregates, has, size, desc,
             round, trunc, floor, ceil, now(), uuid()
string       Unicode-safe text functions
convert      checked value conversions
hex          byte and text hexadecimal conversion
base64       byte and text base64 conversion
sha          standard cryptographic hashes
object       typed reads of Object values
time         Unix time, dates, durations, and time zones
random       random bytes, a Call
history      loading a past version, a Call (§16.12)
modules      mounting and updating module values, Calls (§12.4)
```

The standard namespaces are bound in every module and every library, each to major version 1 of its contract, such as `liasse.string@1`, and a `requires` entry MUST NOT name one, by local name or by contract. Every function of `string`, `convert`, `hex`, `base64`, `sha`, `object`, and `time`, and each rounding function `round`, `trunc`, `floor`, and `ceil` (Annex A.3), is immutable.

`string.trim` removes the leading and trailing Unicode scalar values that carry the Unicode `White_Space` property (equivalently, the scalars Rust's `str::trim` removes); `string.lower`, `string.upper`, and `string.casefold` apply the corresponding Unicode default case and casefold operations.

`string.starts_with(value, prefix)` returns whether the Unicode scalar sequence `value` begins with the exact Unicode scalar sequence `prefix`. The empty prefix matches every value; a prefix longer than the value does not match.

`convert.text(value)` converts an `Int` to its canonical base-10 `Text` spelling: `0` stays `"0"`, a positive value is its digits from its first nonzero digit, and a negative value has one leading `-` before those digits. It converts a `Uuid` to its canonical text: the 32 lower-case hexadecimal digits of its 16 bytes in the 8-4-4-4-12 grouping of RFC 9562, separated by `-`, so `convert.text(uuid())` yields a fresh identifier text. `convert.text` accepts these two source types.

### 13.2 Module requirements

A module declares the components and libraries it uses in `requires` (LANGUAGE §13.3):

```liasse
requires {
    cose: "liasse.cose@1"
    webauthn: "liasse.webauthn@1"
    oidc: "liasse.oidc@1"
    password: "liasse.password@1"
    text: "acme.text@1"
}
```

A library requirement resolves to the library of that name and major that the artifact's `libraries/` provides, and its source is part of the definition (§3.1). Every other entry is a component requirement, which resolves to the one registered component of that contract and major; zero or several such components reject loading before the module becomes active, and the diagnostic names the requirement.
At load time, the context supplies a descriptor for each component requirement, containing:

```text
contract name and major
named value types and templates, with their CBOR codecs
the mixins it offers in the slots of types, with their typed signatures
function and Call names, their typed signatures, and their categories
for each Call, its kinds: generated, provider-backed with the capability it needs, or with a state effect
equality, ordering, and key eligibility of the published types, when provided
```

### 13.3 Functions and Calls

Every operation that a component publishes is an immutable function, a stable function, or a Call (LANGUAGE §8.5), as its descriptor declares; a Call is generated, provider-backed, or with a state effect.

- **Functions.** Functions, immutable and stable, are interchangeable whatever implements them: the kernel, a standard namespace, a user `fn`, or a component. A component function is a function wherever it is used, and the backend evaluates it in every position that uses it.
- **Verifiers.** A verifier validates untrusted input against declared keys or configuration and returns a typed proof, or refuses. A verifier that only reads, checking its input against key material held in the state it reads, is a stable function: the keyring verifier of §14.7 is one. A verifier that needs a remote provider or records state, such as the verification of a passkey assertion against a challenge that it uses up, or of an OIDC response, is a Call. A refusal rejects the transaction that makes the verification with the code `host`, except in an authenticator's `verify`, where it denies the request (§10.5).
- **Results.** A component result that lacks its declared type rejects the call with the code `host` and commits nothing.

### 13.4 Rust registration

The root client registers each namespace when it builds its context:

```rust
let context = LiasseContext::builder()
    .namespace("cose", cose_namespace())
    .namespace("webauthn", webauthn_namespace())
    .namespace("oidc", oidc_namespace())
    .build()?;
```

A namespace receives typed Liasse values and explicitly granted provider handles; application state reaches it only as those values. The types and templates it publishes are named as LANGUAGE §5.4 rule 9 states.

---

<a id="keyrings"></a>

## 14. Keyrings and key providers

A keyring is logical state governing cryptographic key versions and their lifecycle. A key provider is a host-supplied Rust implementation that holds the private key material behind opaque handles and performs protected operations, each a Call. The keyring component records the public side of each key version as state that it alone writes, and the root client gives each keyring its provider, algorithm, and lifecycle policy through a key class.

### 14.1 Keyrings and key classes

A keyring is a member whose definition is the `Keyring` template that the key-management component publishes (LANGUAGE Annex C.2):

```liasse
requires { keys: "liasse.keyring@1" }

session_keys: keys.Keyring
document_keys: keys.Keyring(class: "documents")
```

The `class` argument gives the keyring's **class name**, `default` when it is omitted. The root client resolves each class name to a **key class**, which fixes the keyring's provider, algorithm, and lifecycle policy.

**Key classes.** The root client defines a key class with `key_class(name, KeyClass { provider, algorithm, rotation, overlap, retain, protection })`:

- `provider` names a registered key provider (§14.5);
- `algorithm` names the key algorithm, such as `Ed25519` or `ES256`;
- `rotation` is the cadence of automatic rotation, a duration, or `manual`, for activation by the root client (§14.4);
- `overlap` is the lead time at which automatic rotation exposes a pending version before activating it, zero by default;
- `retain` is how long a retired version stays accepted for verification; when the class leaves it unset, a retired version stays accepted until its revocation or destruction;
- `protection` is the provider protection class that the keyring requires, such as hardware, when the class states one.

`key_class` rejects a class whose `overlap` is not strictly less than its `rotation` cadence, when it has one. Defining a class checks its provider's capabilities (§14.6).

**Bindings.** The root client binds key classes with `bind_keys(address, class)`, which binds the class name `default`, and `bind_keys(address, name, class)`, which binds the class name `name`. The nearest binding wins and is inherited through mount points, as for storage (§15.3): a keyring resolves its class name through the binding at the nearest address that encloses it.

- Load, mount, and update reject a keyring whose class name is unresolved at its instance's root, naming the member and the class name (§8.2, §12.4, §17.3).
- Binding a class checks its provider's capabilities against the keyrings it resolves (§14.6).
- A change of a class, or of the binding that resolves a keyring, applies from the keyring's next rotation: the active version stays active, and the next version comes from the class in effect then.
- An artifact holds the module and its keyrings' recorded versions; the root client holds its key classes and bindings.

A keyring's versions change only through the component's transitions; seeding or writing a keyring member is a static error.

### 14.2 Public keyring view

A keyring exposes public metadata through the views that its template declares:

```text
ring.current     the active version
ring.accepted    the versions accepted for verification
ring.public      the public key values of the accepted versions
ring.versions    every retained version
```

`current` yields at most one version, the active version (§14.3). `accepted`, `public`, and `versions` yield relations; `public` projects the public-key material of the versions in `accepted`.

A version exposes these members:

```text
id
algorithm
public_key
created_at
activated_at?
retired_at?
revoked_at?
attestation?
```

An optional member marked `?` (`activated_at`, `retired_at`, `revoked_at`, `attestation`) is absent, and omitted from its CBOR map, until it holds a value. The provider keeps private key bytes, provider credentials, provider handles, and provider recovery records. Members, views, expressions, wire values, diagnostics, history records, and artifacts carry public-key material and the provider metadata that is safe for application views, and a declaration that would reach private material is a static error.

### 14.3 Version lifecycle

```text
pending -> active -> retired -> destroyed
                     \-> revoked
```

- `pending` has a provider handle and verified public metadata;
- `active` is selected for new operations;
- `retired` remains accepted according to `retain`;
- `revoked` is rejected immediately;
- `destroyed` has completed provider destruction and keeps its public metadata.

At most one signing version is active for a keyring at one admitted state position. A granted member whose operation requires an active keyring succeeds only after that keyring has one active version. During `create`, automatic mode generates and activates the first version as part of genesis; manual mode requires the root client to bind and activate one before an operation that signs can succeed.

A pending version appears in `versions`, and it joins `accepted` and `public` when it is activated. `accepted` holds the active version and each retired version still inside its `retain` window.

The component refuses a provider key whose public material does not match the keyring's key class, such as a key of another algorithm.

### 14.4 Rotation

Automatic rotation creates a new provider key under the keyring's key class, exposes it as a pending version at the class's `overlap` lead time, then activates it and retires the prior active version in one transition. A retired version is destroyed at the provider once its `retain` window has passed. Rotation transitions are maintenance transitions, run with `context.actor` unbound (§10.2).

A retired version's recorded `retired_at` is the scheduled cadence boundary, `retired_at = activated_at + rotation`, with `rotation` taken from the keyring's key class, whenever the rotation runs. A retired version is accepted for verification over the half-open window `[retired_at, retired_at + retain)`; the instant `retired_at + retain` lies outside it.

With manual activation, the root client binds an externally created provider key to the keyring; the component validates its public metadata and activates it through the same transition. Work that cannot be undone, such as destroying key material, starts only once the transition that asks for it is admitted. A key generated or bound for a rejected transition is disabled, and the keyring's versions stay as they were.

### 14.5 Key provider trait

A provider is registered under a name, which a key class's `provider` names:

```rust
context.key_provider("session-hsm", Arc::new(MyKeyProvider::new(...)));
```

A representative Rust contract is:

```rust
#[async_trait]
pub trait KeyProvider: Send + Sync {
    fn capabilities(&self) -> KeyCapabilities;

    async fn generate(&self, spec: KeySpec) -> Result<KeyHandle>;
    async fn bind(&self, external: ExternalKeyRef, spec: KeySpec)
        -> Result<KeyHandle>;

    async fn public_key(&self, key: &KeyHandle) -> Result<PublicKey>;
    async fn sign(
        &self,
        key: &KeyHandle,
        algorithm: &str,
        message: &[u8],
    ) -> Result<Vec<u8>>;

    async fn disable(&self, key: &KeyHandle) -> Result<()>;
    async fn destroy(&self, key: &KeyHandle) -> Result<()>;
    async fn attest(&self, key: &KeyHandle) -> Result<Option<Attestation>>;
}
```

`KeyHandle` is opaque outside the provider and runtime boundary.

### 14.6 Provider capability checks

Defining a key class checks that its provider advertises:

- the class's algorithm and key type;
- automatic generation, or external binding for manual activation;
- the class's protection class, when it states one;
- the disable, destroy, and attestation behavior that the class's policy requires.

Binding a class, and loading a definition whose keyrings it resolves, checks that the provider supports every protected operation that a call site performs on those keyrings, such as signing through a COSE namespace (§14.7). An incompatible provider rejects the class, the binding, or the load with a capability diagnostic that names the class, the provider, and the missing capability.

### 14.7 Restricted namespace access

A COSE namespace MAY sign through a keyring. Signing is a provider-backed Call, admitted in a transaction (LANGUAGE §8.5):

```text
cose.sign(/session_keys, claims)
```

It signs with the keyring's active version, through the provider, and the private bytes stay with the provider (§14.2).

Verification uses the accepted public versions:

```text
cose.verify(/session_keys, token)
```

It is a keyring verifier: a stable function over the accepted public versions in committed state, admitted in every function position and in an authenticator's `verify` (§10.4). It reads committed state only, so it keeps working while the provider is unavailable. The result includes the verified key-version identity.

### 14.8 Direct session token construction

A mutation can construct and return its own token:

```liasse
role member(auth: /session, members: /accounts[.enabled]) {
    mut refresh() {
        let row = /sessions[.id == context.session.id]
        row.expires_at = now() + time.duration("P30D")
        let token = cose.sign(/session_keys, { auth: "session", session: row.id, exp: row.expires_at })
        return { auth: "session", token, expires_at: row.expires_at }
    }
}
```

### 14.9 Provider failures

A provider failure before admission rejects the affected operation, with the code `host`, and commits no application effect. When scheduled rotation cannot create or validate a replacement, the current version remains active and the runtime reports the overdue rotation through health and diagnostics. Operations using that version continue when the provider can perform them; an unavailable signing operation rejects the requesting mutation.

Key activation, retirement, revocation, and destruction are transitions of the keyring's state.

---

<a id="blobs"></a>

## 15. Blobs

A blob is binary content held outside the state of a module instance and named in that state by a typed descriptor. The root client decides where content lives (§15.3).

### 15.1 Values

`Blob` is the kernel type of a blob descriptor, a record of four members:

```text
digest   Bytes    the 64-byte SHA-512 digest of the content
size     Int      the non-negative byte count of the content
media    Text     the canonical media type of the content
name     Text?    an optional file name
```

Its CBOR form is a map (Annex A.4):

```cbor-diag
{
    "name": "invoice-113.pdf",
    "size": 184320,
    "media": "application/pdf",
    "digest": h'd03738eebf3eaaaef04c2267f349c134a1024f4aaa99cfd3c2326925fddda7c6e583f7bd31905234390621fbc3c27090032f7f9f5ab61de78a2d9e7f5a7dd0d0'
}
```

- The digest is the content's identity: it names and verifies the bytes. The complete descriptor is the value, so two descriptors that name the same content with different media types or file names are different values.
- A `digest` whose length differs from 64 bytes, a negative `size`, and a `media` outside the canonical media-type form (Annex A.4) make the descriptor malformed.

### 15.2 Uploads

A blob argument is a typed input. Constraints on its type state which blobs a mutation accepts (LANGUAGE §6.3 rule 3), and admission judges them on the declared descriptor before it receives any content. The client sends the descriptor in the argument map and the content beside the call (§11.1). The runtime verifies the content against the descriptor's `size` and `digest`, and a disagreement rejects the call before its transition is admitted, as soon as the content exceeds the declared `size`.

A transaction commits only once the storage class of every member into which its complete result writes a descriptor durably holds the content: the number of verified copies that the class requires (§15.3). A transaction whose content the class cannot hold is rejected with the code `host`, naming the member.

### 15.3 Classes and bindings

A stored `Blob` member names where its content belongs with its storage name, `default` unless its type is written `Blob(storage: "name")` (LANGUAGE §5.4 rule 8).

The root client maps storage names to **storage classes**. A storage class is host configuration: an ordered list of locations, each a registered connector with its configuration and secrets, and a number of copies. The root client defines a class with `storage_class(name, class)` and binds classes with:

```text
bind_storage(address, class)         binds the storage name default at address to class
bind_storage(address, name, class)   binds the storage name name at address to class
```

- An address is the display path of the root, a row, or a mount point (Annex C.2).
- A stored blob resolves its member's storage name through the binding at the nearest address that encloses it: its own row, each owner outward to its instance's root, then through the mount point into the parent instance, up to the application root. The nearest binding wins.
- `bind_storage` accepts only a class that `storage_class` defines. Load, mount, and update reject a definition that uses a storage name unresolved at the instance's root, naming the member and the storage name (§8.2, §12.4, §17.3).
- Content satisfies its class when as many of the class's locations as its copies hold a verified copy.
- When the class that a member's content resolves to changes, through a binding, a change of a class's locations or copies, or an update that changes the member's storage name, the runtime moves the content to satisfy the new class in the background. State and history stay as they are, and reads keep working while content moves.

### 15.4 Reads

A currently authorized granted view that shows a `Blob` value grants fetching its content; a view that shows only some members of a descriptor grants no fetch.

A `fetch` request names the granted view, the occurrence token of the row that holds the blob for a view that yields rows, and the path of member names down to the `Blob` (§11.1). The **fetch plan** lists the descriptor and the sources that hold a verified copy, in the order of the class's locations, each with connector-specific access data, such as a signed address and its expiry. Access data MAY expire. A client MUST verify the complete content against `digest` and `size` before it uses the bytes.

When the runtime transfers content itself, it delivers verified content alone, and the fetch fails with the code `unavailable` when no source delivers it.

**Content reads.** A Call that takes a blob, such as `modules.mount` and `modules.update` with their artifact (§12.4), receives its verified content. The read first uses content that the calling transaction received for an argument with the same descriptor (§15.2). When no source delivers verified content, the Call fails and rejects its transaction with the code `host`.

**Verification.** Content is verified when every byte matches the digest and its byte count matches `size`.

### 15.5 Retention

The backend keeps content while live state or retained history reaches it. A descriptor in live state keeps its content in the copies that its class requires. A descriptor that only retained history holds keeps its content in at least one verified copy, so every retained point stays restorable (§16.4).

### 15.6 Connector contract

A connector is a Rust component that implements one external storage system. The root client registers it under a name, matched exactly, and a storage class's locations name it:

```rust
context.blob_connector("s3", Arc::new(S3Connector::new(s3_settings)?));
```

A representative Rust contract is:

```rust
#[async_trait]
pub trait BlobConnector: Send + Sync {
    async fn put(&self, at: &Location, digest: &Digest, content: ContentStream) -> Result<()>;
    async fn get(&self, at: &Location, digest: &Digest, range: Option<ByteRange>)
        -> Result<ContentStream>;
    async fn access(&self, at: &Location, digest: &Digest, expires_in: Duration)
        -> Result<Option<AccessData>>;
    async fn copy(&self, from: &Location, to: &Location, digest: &Digest) -> Result<()>;
    async fn delete(&self, at: &Location, digest: &Digest) -> Result<()>;
    async fn usage(&self, at: &Location) -> Result<Usage>;
}
```

The runtime verifies every result of a connector before it counts, records, or delivers it.

---

## Part IV — History and application lifecycle

<a id="history"></a>

## 16. History, artifacts, and reconciliation

History preserves the committed states of every module instance.

### 16.1 History ownership and composition

Every module instance, the root module and each mounted child alike, owns one independent history covering:

- its definition: its module file and the libraries it was built with, the prelude included (§3.2);
- its writable state;
- the mounting, updating, and unmounting of the modules mounted at its mount points (§12.3, §12.4);
- the logical effects of every committed transition within its boundary.

Every committed change of stored state is a version in the history of the instance that owns the state. A parent history records which child incarnation is mounted at each of its mount points and which point of that child's history each parent point selects; the child's own deltas stay in the child's history. A transaction affecting several module instances is represented in each affected history with one shared transaction identity.

### 16.2 Serial order

History follows the serial order of admission (§19.1), and replaying it reproduces the committed state of every point. Once a history point has been exported, its identity and its position relative to the other points of its lineage remain stable.

A point is a **consistent state** of the serial order: a prefix of it. Every commit that changes an instance's definition, mounted children, or owned state is reflected by exactly one point of that instance's history, the first point of its lineage that includes it; the commits that a point reflects and its previous point does not are the point's commits, and the point records their transaction identities. A point MAY reflect several commits that ran concurrently. A point reflects whole commits: a transition, and a transaction shared by several instances, is in a point entirely or not at all, in each history it touches.

### 16.3 History points, lineages, and branches

A **history point** identifies one exact retained state of one module instance: its definition, the children mounted at its mount points, and its owned state, as of the commits it reflects. Each point except a genesis point has a **previous point**. A **lineage** is an ordered continuation of points from genesis or from an earlier point: its **origin** is genesis or the point it continues from, and its **head** is its latest point.

Point, lineage, and transaction identifiers are opaque. An implementation MUST give distinct identifiers to distinct points, and to distinct lineages, of one instance, including points and lineages that two stores create independently from copies of one artifact.

A point `p` **precedes** a point `q` when `p` comes before `q` on `q`'s lineage, or when `p` is, or precedes, the origin of `q`'s lineage. Following lineage origins from any point leads back to a genesis point.

Each instance has exactly one **selected point**, the point that its live state is. The lineage of the selected point is the **active lineage**, and every other retained lineage is an **alternate lineage**, also called a **branch**. The selected point is the head of the active lineage, except right after a rollback. A commit extends the active lineage in place when the selected point is the head of that lineage and the lineage was started in the store that holds the instance; otherwise the commit starts a new lineage whose origin is the selected point. A commit's place in the serial order decides which lineage it extends.

A **composition point** selects one point for an instance and one point for each child mounted at it, recursively. A composition point is **coherent** when every transaction recorded in two of the selected histories is included by the selected point of each, or by neither. The **active composition**, the selected point of the root and of every mounted child recursively, is always coherent.

### 16.4 Retention

The backend retains the complete history of every instance: every point of every lineage, from genesis on. A history operation, an export, or a `history.at` call MAY wait for the retrieval of past states; it takes effect when the retrieval completes, and it is rejected when the retrieval fails.

A retained point is **restorable** when its exact state can be reconstructed from the retained records and the content of every blob that its state reaches is available (§15.5). A movement selects only a restorable point.

### 16.5 Dehydrated modules

Dehydrating an instance writes it as a dehydrated module (§3.1, Annex C.3). It names each child mounted at it by the child's **mount path**, the display path of the mount point relative to the instance (Annex C.2), such as `/billing` or `/companies/k7Qx2m/billing`.

Each artifact is complete in itself: a child's definition, libraries, chain, state, and history are in its own nested artifact, also when the parent's definition uses the same libraries. A nested artifact is the dehydrated module that dehydrating the child alone at the same boundary produces.

Hydrating stores the carried blob content under the storage classes that the root client binds for the instance (§15.3) before the hydrated state becomes visible.

### 16.6 Portable history records

`history.cbor` carries the history of the instance, one record per point with its delta from its previous point (Annex C.4). A reader reconstructs every carried point from the checkpoint in `data.cbor` by applying deltas, in either direction, along the ancestry of §16.3. A carried point is restorable from the artifact when its deltas connect it to the checkpoint and the artifact carries the content of every blob that its state reaches (§16.4).

### 16.7 Export boundary and scope

An export is a read at one committed application boundary, a consistent state of the serial order, as a frontier is: the root state, the carried histories, the composition point, and the child states, recursively, describe that same boundary.

Exporting an instance dehydrates it and its children, recursively (§16.5). An export carries the complete retained history of every instance, or the part that the root client selects: the active lineage, additional lineages, or a range of points. Each carried point's version is a version of the exported chain, from its floor up (§3.1); the points of other versions stay in the store's retained history. The selected point of every instance is restorable from the artifact, and the artifact carries the content of a blob that only other carried points reach where that content is available. An export fails when it cannot make every selected point restorable, and when its selection names an instance, a lineage, or a point that the store does not hold.

### 16.8 Import and classification

Import compares each incoming instance whose incarnation matches a local instance with that instance's retained history, from the incoming selected point `I` and the local selected point `L`, through the precedes relation of §16.3 over the union of the two histories:

```text
same point                            already synchronized
local point precedes incoming         fast-forward available
incoming point precedes local         rollback available
shared point followed by divergence   divergence: rejected with a divergence report (§16.9)
no shared point                       unrelated: replacement when the policy permits it
```

A point is **shared** when both histories hold it. The **latest shared point** is the shared point that precedes both `I` and `L` and is preceded by every other such point. An incoming instance whose incarnation matches no local instance is unrelated to every local instance; a child of that kind is a mount that the incoming side added, which the parent's classification covers.

An **import policy** lists the movements that an import activates by itself when its classification calls for them: **fast-forward**, **rollback**, and **unrelated replacement**. For every other classification, the import reports the classification, and the local selected point and state stay as they are.

A permitted movement is applied as follows:

- **Fast-forward** adopts the incoming points after `L`, with their identities, their lineages, and their recorded values, and selects `I`.
- **Rollback** selects `I` as a local rollback does (§16.11).
- **Unrelated replacement** makes the incoming instance's identities, selected state, and history active, as a restore would make them in an empty target. When the two instances have the same incarnation, the local lineages stay retained beside the incoming ones. When the replaced instance is a child with another incarnation, the parent's history records the replacement as a change of the mount, and the replaced child's history stays retained. When it is the root with another incarnation, the application becomes the incoming instance, and the import report states the replacement.

Every movement adds the points and lineages that only the artifact holds. A point that the local history already retains keeps its local records, whatever the artifact carries for the same identifier. The blob content that the newly selected state reaches is brought in before the activation becomes visible.

Each instance classifies within its own boundary, a matching child inside its own nested artifact, under the same policy; mounting, updating, and unmounting a child are commits of the parent, which the parent's classification covers. Import then builds the complete prospective composition and validates it as every movement is validated (§16.11), a coherent composition point included. A divergence, a failed movement, or an invalid composition rejects the whole import; otherwise every instance's movement activates in one transition.

An import is a transition of the root client. It ends as `committed` when it changed a selected point, as `unchanged` when the classification was same point or its movement was outside the policy, and as `rejected` when reading the artifact (§3.1), validation, or a divergence stopped it. Its **import report** gives, for each instance, the classification and whether a movement changed its selected point, and, for a divergence, the divergence report.

### 16.9 Divergence

The **divergence report** of an import holds, for every diverging instance:

- the instance's incarnation and mount path;
- the latest shared point `S`, the local point `L`, and the incoming point `I`, with the version in effect at each;
- every coordinate whose value at `L` differs from its value at `S`, and every coordinate whose value at `I` differs from its value at `S`, each with its values at `S`, `L`, and `I`;
- the coordinates that both sides changed, whether to equal or to different values;
- the child mounts that each side changed, and two different child incarnations at one mount path;
- for each diverging child, its own report, recursively.

A coordinate is named by its display path relative to the instance root (Annex C.2): a row by its identity text at each level, a mounted child by its mount path, and a member of the root, or of a static struct at the root, by its names alone, such as `/settings/page_size`. Each side's values are stated under the definition in effect on that side.

The root client resolves a divergence with `branch` and `switch` (§16.11) and with native transactions (§20.1).

### 16.10 Round-trip and operation records

Restoring an artifact and exporting the same instance boundary, selected points, and retained history reproduces the same definitions, libraries, migration chains, owned logical states, lineages, transaction associations, module-instance closure, and blob content.

Operation records (§11.3) stay as they are through history movements: a call retried after a rollback reports the outcome of its first execution, even when the rollback displaced that commit, and it runs once. They stay in the store that holds them: after a restore in another store, the status of an operation identifier used before the export is `unknown`, and a request retried with it is a new operation.

### 16.11 History movements

A **history movement** changes which retained point an instance is at. Movements are operations of the root client (§8.2):

```text
rollback(instance, point)       select a retained point that precedes the selected point
fast_forward(instance, point)   select a retained point that the selected point precedes
switch(instance, lineage)       select the head of another retained lineage
branch(instance, artifact)      retain an artifact's history as alternate lineages, keeping the selected point
```

An instance is named by the root or by the handle that native reflection gives a mounted child (§20.1), and points and lineages by their opaque identifiers. Every movement:

- runs with native authority, outside every role and actor, and records host provenance (§20.1);
- is admitted as one transition, and live views advance through it like through any commit;
- selects a point that history already retains;
- keeps every point and lineage of the instance it moves: the continuation it leaves stays retained as a lineage;
- builds the complete prospective composition before it activates anything, and validates it like any activation of a definition: every definition it selects is resolved under its language version, every component such a definition requires is available, every interface between a mount point and its child holds (§12.2), every constraint holds, and the composition point is coherent. Any failure rejects the movement;
- adopts the recorded state of the points it selects;
- ends as `committed`, as `unchanged` when the target is already selected, or as `rejected` with diagnostics (§8.4).

A movement of an instance moves its whole subtree in one transition: selecting a point `p` selects the composition point at `p`, the composition that was active while `p` was selected, so each child mounted at `p` moves to the point that composition selects for it, recursively, and a child mounted now and absent at `p` is unmounted. Rolling back past a mount unmounts that child, whose instance and history stay retained in the parent's history; rolling back past an unmount mounts the child again, with the incarnation and the point it had then. A movement of a child alone MUST keep whole every transaction that the child shares with an instance outside its subtree; a movement that would split one is rejected with a diagnostic that names the shared transaction and the other instance. Every child point that a movement selects MUST be restorable under the child's own records; otherwise the movement is rejected with a diagnostic that names the child and its point.

`rollback` restores the instance exactly to the target point: its definition, an earlier version when the target precedes an update, its mounted children, and its state. A target that does not precede the selected point rejects it. `fast_forward` selects a point that the selected point precedes, such as the head of a continuation that a rollback displaced while no commit has started a new lineage since; selecting the head of another lineage is a switch. `branch` reads the artifact (§3.1), requires it to represent the same instance incarnation, and retains every incoming lineage and point that only the artifact holds, keeping the selected point. It does so for every instance of the artifact, recursively: the lineages and points of each child whose incarnation matches a local child, and the instance and history of each incoming child that no local instance matches, retained as a child that the parent's history mounts, so that a later `switch` to an incoming lineage finds every child point that its composition selects. It ends as `committed` when it retained anything and `unchanged` otherwise.

After a movement, the runtime closes every live subscription whose view the selected definition removes or stops granting to its session.

### 16.12 History in programs

Every stored row has the metaproperty `$history` (LANGUAGE §8.3 rule 13). `created_at` is the commit time of the transaction that inserted the row; `updated_at` is the commit time of the latest transaction that changed the row's own stored fields, the fields of its static structs included. Both members follow the row's own fields alone: a change inside a nested table of the row, and a write that leaves every field equal, leave them as they are. The commit time of a transaction is the instant that `now()` returns in it (§19.2). A transaction reads `$history` in its intermediate state, like every value: a row it inserts reads the transaction's instant for both members.

A program loads past versions into live state through `history.at(source, instant: Timestamp)`, a Call (§13).

- `source` is a stored table of the calling module, or a selection of its rows. The call yields those rows as they stood in the state of the active lineage that was selected at `instant`, the state of the latest commit at or before `instant`; a table source includes its rows deleted since.
- The result is a relation of records `{ row: R; history: History }`, where `R` is the record of the table's current stored members and `History` the type of `$history`. `row` holds the row's values at that state, carried to the table's current definition as an update carries them (§17.1). The relation holds only the rows whose carried values fill every required member of `R`, a member added after that state filling from its initializer. `history` is the row's `$history` at that state. The records are values: writing them into live state is an ordinary write of the program, and the program relates them to live rows through their values, such as a member that a `unique` covers.
- The call yields an empty relation when `instant` precedes the instance's genesis.
- `history.at` reads the calling module's own history; a mutation of a mounted child loads that child's past versions.

---

<a id="evolution"></a>

## 17. Package evolution and migrations

A module evolves through versions, each a complete definition, and the migration files of LANGUAGE §14 join consecutive versions into its migration chain (§3.1). An update moves an instance along that chain, up or down, in one commit. A runtime applies the build check of LANGUAGE §14.3 to every migration on an update's route, against the definitions that the route uses (§17.2), before the update activates; a failure rejects the update and names the path.

### 17.1 Applying a migration step

One step of a route applies one migration in one direction, or, between consecutive versions with equal data structures, carries the state as it is to the next definition. A step in the up direction applies its entries as written; a step in the down direction applies their inverse: an `add` removes the declarations it names, a `remove` recreates the declarations it restates, a `rename` renames back, and a `retype` maps through its `down` expression. Within a step:

1. **Structural delta.** A renamed declaration continues the previous one with its identity. Every row that the step carries keeps its row identity, its values, its `$history`, and its insertion order, a row of a renamed table included, and a renamed mount point keeps its mounted child with its incarnation. A removed member's values and a removed table's rows leave live state; a removed mount point unmounts its child (§18.2). A member of unchanged type copies its values as they are; a value changes type only through a `retype`. A retyped member's value at each place is the result of the direction's mapping, evaluated with `old` bound to the previous value, `none` where it was absent; a mapping that fails to evaluate, or whose result does not have the target type, such as `none` for a required target, rejects the update, naming the path and the address of the place.
2. **Values that enter.** An added table starts empty. At every place that enters live state, an added field takes its initializer, evaluated at the update as at any insertion and recorded with the update commit (§7.9), or stays absent when it is optional; the program writes each other required field.
3. **Program.** The direction's programs run over the result in the order of the file, a scope's programs at the scope's position, each over the intermediate state that the programs before it leave, all in the update's one transaction, with `.` and `old` bound as LANGUAGE §14.3 rule 5 states. `drop` has no effect at run time.

The steps of a route apply in order, each reading the previous step's result as `old`. The result of the last step is judged and committed (§17.3); the intermediate states exist only within the update.

### 17.2 Routes

A runtime resolves the route of an update from the instance's active version `c` to the incoming artifact's version `t`:

- The available chain is the incoming artifact's chain together with the chain that the instance retains from its active package; the retained chain supplies the versions and migrations above an older incoming version. Where both carry one version or one migration, the incoming artifact's is used.
- For an upgrade, `c` below `t`, the route crosses every consecutive pair from `c` up to `t` in ascending order, applying each migration's up direction. For a downgrade, `c` above `t`, it crosses every consecutive pair from `c` down to `t` in descending order, applying each migration's down direction.
- No route exists when `c` is not a version of the available chain, such as a version below its floor, or when the data structure of the instance's active definition differs from the data structure of the chain's definition at `c`. The update is then rejected, naming the instance's version, and the floor when `c` lies below it, and the active definition remains active (§8.4).
- When `c` equals `t`, the route is empty, and the incoming definition becomes active over the unchanged state.

### 17.3 Updates

The root client updates the root module (§8.2). A transaction updates a mounted child with `modules.update(.m, artifact: Blob)`, where the blob holds a package (§12.4). Both apply the same semantics:

- **Validity.** An update is one commit, validated like every activation (§8.2), the context that each mount point provides against its child's `mount` declaration included (§12.5). A missing route (§17.2), a failed step (§17.1), or an invalid result rejects it, and the prior definition and state remain active (§8.4).
- **Scope.** An update migrates the state that the updated instance owns.
- **History.** The update commit, a downgrade's included, is an ordinary commit on the active lineage.
- **Parents and children together.** A parent and its children MAY be updated in one transaction, whose combined result is judged once: a mutation MAY call `modules.update` on several of its mount points, and the root client MAY combine the root module's update with updates of mounted children in one transition.
- **Activated definition.** From the update commit on, the activated definition governs, and the instance retains its package and chain for later routes (§17.2).

---

<a id="deletion"></a>

## 18. Deletion and unmounting

Deletion removes a row from live state, together with the rows nested in it and the modules mounted in it. Unmounting takes a module instance out of live state. History keeps every committed value of both.

### 18.1 Deletion

These writes delete rows (§7.5, LANGUAGE §10):

- `-=` on a table, a whole-table replacement, and `= none` on an optional table;
- the removal of an element from a `Set` or of an entry from a `Map`, whose rows are part of their owner;
- a migration step that removes a table, and a statement of a migration program that deletes (§17.1);
- a root-client native transaction (§20.1).

Deleting a row removes, in the same statement, every row nested in it, recursively, the rows of its element-form tables included, and unmounts every module mounted at its mount points (§18.2). Rows relate by nesting and by values, so a deletion changes the deleted rows alone. A row that relates to a deleted row by value, such as one holding the deleted row's `id`, keeps its values.

A deletion is judged with the rest of its transaction. Every `assert` constraint whose condition reads a row that the transaction deletes is judged again on the complete result (LANGUAGE §6.3 rule 5), so a deletion that leaves such a constraint false rejects the whole transaction with the code `constraint`. A constraint thus forbids a deletion while dependent rows exist, and `delete_project` removes the dependents with the target:

```liasse
projects: Table {
    identified()
    name: Text
}

tasks: Table {
    identified()
    project_id: Uuid
    title: Text
    assert(count(/projects[.id == ^.project_id]) == 1, "A task belongs to an existing project")
}

mut delete_project(id: Uuid) {
    .tasks -= .tasks[.project_id == id]
    .projects -= .projects[id]
}
```

- **Planning.** The removals of one statement are computed as a whole, against the transaction's intermediate state before the statement: the rows that the statement removes directly, each once, their nested rows, and the modules mounted in any of them. The complete set is then removed atomically.
- **Identity.** A deleted row's identity ends and stays unique to that row. Inserting a row with equal values creates a new identity.
- **History.** A rollback to a point before the deletion returns the rows to live state with their identities and values (§16.11).

### 18.2 Unmounting

A module is **unmounted** when the value of its mount point is removed: by assigning `none` to an optional mount point, by deleting the row that holds the mount point or an owner of that row, or by a migration step that removes the mount point (§12.4). Unmounting:

- takes the child's state out of live state, with the children mounted in it, recursively, in the same transaction as the write that caused it;
- ends the child's incarnation: mounting again at the same mount point creates a new instance with a new incarnation and a new history;
- keeps the child's history retained as part of the parent's retained history, as a child that the parent's history once mounted; a rollback of the parent past the unmount mounts the child again with its incarnation and the point it had then (§16.11);
- is recorded in the parent's history as a change of the mount, in the transaction's point;
- removes the instance's contexts: live views that read through the mount point update at the frontier of the commit, a session mounted at a context of the removed instance has its requests denied and its live views closed with `unauthorized` (§11.2), and a context binding of such a context points at a removed instance until the root client binds it again (§10.1).

---

## Part V — Runtime and host contract

<a id="runtime"></a>

## 19. Runtime semantics

### 19.1 Concurrent and serial ordering

Admission is serializable. Every admitted request occupies one position in a serial execution order, with the guarantees of PostgreSQL `SERIALIZABLE` isolation (serializable snapshot isolation, SSI): the committed state is the state that running the admitted requests one at a time, in that order, produces.

A transaction observes every commit ordered before it. A transaction that starts after a commit was acknowledged, to any client and on any connection, observes that commit. Two concurrent requests MAY commit in either relative order.

Each request's assertions are judged at its serial position. Of two concurrent withdrawals, one MAY succeed and the next fail:

```liasse
mut withdraw(amount: Decimal) {
    assert(.balance >= amount, "Insufficient funds")
    .balance -= amount
}
```

### 19.2 Time and recorded timestamps

`now()` follows transaction-timestamp semantics: one best-effort wall-clock instant, in microseconds, is fixed for the complete external request, load, update, migration, history operation, or system transition, and every `now()` call in that operation returns that same instant.

Commit order is admission order (§19.1). A later commit MAY hold an earlier timestamp when the host wall clock moves backward.

### 19.3 Temporal evaluation

The runtime evaluates every view whose result depends on `now()` from its best available wall clock, in microseconds. Wall-clock movement in either direction MAY cause rows to enter or leave such a view, as in `view active_sessions = .sessions |> filter(now() < .expires_at)`; the runtime MUST reflect the resulting current logical view and emit a new live frontier in the order the temporal observation is established. A temporal frontier advances live views alone: state and history stay as they are.

The calendar operations of the `time` namespace use the backend's time-zone database.

---

<a id="rust-host"></a>

## 20. Rust host and implementation contract

The root client is the Rust program that embeds Liasse as a library. It supplies the components, key providers, blob connectors, storage and key classes, transports, and context bindings of the application.

### 20.1 The root client

The root client has native access to the whole tree. It introspects every module — its definition, its interfaces, its state, and the modules mounted in it — and composes **native transactions** through the Rust API: the dynamic path, where a mutation is the stored path. Native access follows these rules:

- **Parity.** Whatever a native transaction does, a mutation can do, and the reverse, within the mutation's context: a native transaction reads and writes the state of any module at that module's context, calls its mutations and its Calls, and mounts, updates, and unmounts modules; a mutation does the same within its own context and the modules mounted in it.
- **Admission.** A native transaction is one transition, admitted like every request: it holds one serial position, runs over one snapshot with read-your-writes, and commits atomically or not at all. It bypasses context bindings, authenticators, and roles, and it retains every other rule: the types and constraints of every module it writes, the interfaces between mount points and their children, component contracts, serial admission, and atomicity.
- **The root module.** The root client alone manages the root module itself: it creates, opens, restores, and updates it (§8.2), and it alone runs history operations (§16.11) and imports and exports artifacts (§16.7, §16.8).
- **Context bindings.** The root client makes the context bindings at which web clients MAY mount sessions (§10.1).
- **Class bindings.** The root client defines and binds storage classes and key classes (§14.1, §15.3), and it reads the copy status of blob content and the usage of each storage class.
- **Reflection.** Native reflection gives the root client the handle of every mounted instance and the display path of every row and mount point (Annex C.2).
- **Provenance.** A native transaction that changes the application creates an ordinary commit with host provenance recorded separately from the application's actor binding.

### 20.2 Bounded completion

Every admitted request MUST reach a terminal outcome — committed, unchanged, rejected, or denied — in bounded time. The runtime bounds every component call that admission makes, to a namespace, a key provider, or a blob connector; a call that exceeds its bound is a component failure that rejects the request (§14.9).

### 20.3 Diagnostics

Every rejection carries a **diagnostic** of four members:

- `code`, one of the codes below, each with one meaning;
- `message`, a human-readable explanation; a failing `assert` gives its own message or the generated one (LANGUAGE §6.3 rule 7), and that message is application-visible;
- `path`, the display path (Annex C.2) of the state or declaration at fault;
- `trace`, the stack from the request to the failure across its transaction, outermost first: the request's call, with its index in a transaction (§11.1); each mutation call, Call, and migration program on the way, calls through mount points included; and the failing statement, constraint, or expression. Each frame carries its declaration's display path and its source span, and a frame that a template or mixin brought carries the applications it was inlined through (LANGUAGE §11.3 rule 13).

| Code | Meaning |
| --- | --- |
| `malformed` | a request, an argument, a token, or a received value does not parse against its declared form (§11.1, Annex A.4) |
| `constraint` | a body `assert`, or the `assert` of a mutation input's type, failed (LANGUAGE §6.3 rule 7) |
| `uniqueness` | two participating rows of one owner hold one value of a `unique` (§4.3) |
| `assertion` | an `assert` statement of a program failed (§7.6) |
| `evaluation` | an evaluation failed: a zero divisor, a position that requires one row and finds none or several, a value beyond a limit, or an unbound read of `context.actor` or `context.session` (§5.1, §5.2, Annex A.3, Annex A.4) |
| `host` | a component, a key provider, or a blob connector failed or refused, or a storage class could not hold or deliver blob content (§13.3, §14.9, §15.2, §15.4) |
| `conflict` | an operation identifier was reused with a request that is not equivalent (§11.3) |

---

## Part VI — Normative annexes

---

## Annex A — Types and CBOR values

This annex is normative. Every value that leaves or enters an application — on the wire and in artifact records — is one CBOR data item (RFC 8949), in the form that A.4 gives for its type.

### A.1 Primitive types

| Type | Meaning |
|---|---|
| `Text` | Unicode scalar sequence |
| `Bool` | Boolean |
| `Int` | arbitrary-precision integer |
| `Decimal` | exact decimal |
| `Bytes` | small binary value |
| `Uuid` | 128-bit UUID |
| `Date` | Gregorian calendar date |
| `Timestamp` | instant: signed count of microseconds since `1970-01-01T00:00:00Z`, as in PostgreSQL |
| `Duration` | elapsed time: signed count of microseconds, as in PostgreSQL |
| `Object` | dynamic value over the CBOR data model |
| `Blob` | binary-content descriptor |
| `none` | absence of an optional value, represented by position |

An `Enum` value (LANGUAGE Annex C.1) is a refinement of `Text` and is its label's text string. A value of a type that a component publishes has the encoding, equality, and order that the component's descriptor defines.

`none` marks the absence of an optional value, and it is represented by *position*:

- **optional map member** (a struct, record, or row member, a mutation input, a view parameter, a member of a message): `none` is the member **omitted**; a present member is a present value.
- **element of a table with an element form** (a `Set` element, a `Map` key or value): every element is present (LANGUAGE §4.3 rule 15). A required table always has a value: an empty one is the empty array, and an omitted required table member of a received map is the empty table.
- **tuple place**: a tuple is an array of its places, and an absent place is `null` there.
- **a place that cannot be omitted**, such as each element of the `"results"` array of a one-shot transaction (§11.1): `none` is `null`, and so is the result of a call whose mutation declares no response. A one-value result carried as a map member, such as `"result"` or `"value"`, is omitted when it is `none` (§11.2, §11.3).
- **inside an `Object`**: a map member that would hold `none` is omitted, and an array holds present values only.

`null` appears only in the positions this list names; anywhere else it is malformed.

`Text` is a sequence of Unicode scalar values, the code points U+0000..U+D7FF and U+E000..U+10FFFF. Every scalar value, U+0000 (NUL) included, is a legal `Text` scalar, stored and returned exactly. A text is kept exactly as written: a text written with a precomposed character and one written with a combining sequence are two different texts.

A `Decimal`'s value is its numeric value: `1.5` and `1.50` are one value. Its **minimal-scale** form is the decimal fraction `[e, m]` whose mantissa `m` is an integer not divisible by 10, with zero as `[0, 0]`: `1.5` is `[-1, 15]`, and one hundred is `[2, 1]`. A decimal's text in diagnostics is its minimal-scale plain form: every digit in positional notation; trailing fractional zeros removed, and the decimal point dropped when no fractional digit remains; a single leading `-` for a negative value; zero is `0`. Trailing zeros of the integer part are magnitude: one hundred is `100`.

### A.2 Type descriptors

The session manifest (§11.1) describes a type by its canonical text: its type expression (LANGUAGE §5) after elaboration, with every argument written out, such as `Text?`, `Set(of: Text)`, or `{ id: Uuid; title: Text }`. A blob's type text is `Blob`: its storage name belongs to the member (§15.3).

### A.3 Decimal semantics

Arithmetic on `Int` and `Decimal` is PostgreSQL `numeric` arithmetic. Decimals are exact base-10 values, and addition, subtraction, and multiplication are exact.

- `Int / Int` is integer division, which truncates toward zero: `7 / 2` is `3`, and `-7 / 2` is `-3`.
- A division with a `Decimal` operand is PostgreSQL `numeric` division, and its result is a `Decimal`. A terminating quotient is exact: `10.0 / 4` is `2.5`. A non-terminating quotient has at least 16 significant digits, its last digit rounded half away from zero: `1 / 3.0` is `0.33333333333333333333`, and `2 / 3.0` is `0.66666666666666666667`.
- `avg` converts its inputs exactly to `Decimal`, divides as above, and returns `Decimal?`.
- The remainder operator `%` applies to `Int` and `Decimal`: `a % b` is `a − trunc(a ÷ b) × b`, where `a ÷ b` is the exact quotient. The remainder takes the sign of the dividend, matching PostgreSQL `mod`: `-7 % 2` is `-1`, `7 % -2` is `1`, and `5.5 % 2` is `1.5`.

A zero divisor in `/` or `%` is a typed evaluation error that rejects the containing evaluation — view, constraint, or mutation — with a diagnostic.

**Rounding functions.** The kernel rounding functions are immutable and have their PostgreSQL meanings. Each takes an `Int` or a `Decimal` `x` and returns a value of the type of `x`. `digits` is an `Int`; a negative `digits` rounds to a position left of the decimal point.

| Function | Result |
| --- | --- |
| `round(x, digits = 0)` | `x` rounded to `digits` fractional digits, a halfway value away from zero |
| `trunc(x, digits = 0)` | `x` rounded toward zero to `digits` fractional digits |
| `floor(x)` | the greatest integer less than or equal to `x` |
| `ceil(x)` | the least integer greater than or equal to `x` |

`round(2.5)` is `3`, `round(-2.5)` is `-3`, `round(1.245, 2)` is `1.25`, and `round(1250, -2)` is `1300`. `trunc(-2.7)` is `-2`, and `trunc(2.789, 1)` is `2.7`. `floor(-2.5)` is `-3`, and `ceil(-2.5)` is `-2`.

### A.4 CBOR encoding

A writer encodes each value in the form that the table below gives. A reader decodes every well-formed item of that form to its value, whatever the widths of its integers, lengths, and tag arguments, the order of its map keys, the order of the elements of a set or a map, and the scale of its decimals.

The items are integers, byte strings, text strings, arrays, maps, the simple values `false` and `true`, the simple value `null` only where A.1 places `none`, and the tags 2, 3, 4, 37, and 1004 only where this table places them. Numbers are integers and decimals.

| Type | CBOR |
| --- | --- |
| `Text` | text string |
| `Bytes` | byte string |
| `Bool` | `true` / `false` |
| `Int` | integer (major type 0 or 1) from −2^64 through 2^64 − 1; outside that range, a tag 2 (non-negative) or tag 3 (negative) bignum over a byte string |
| `Decimal` | tag 4 decimal fraction `[exponent, mantissa]` |
| `Uuid` | tag 37 over a 16-byte byte string, the UUID's bytes in RFC 9562 order |
| `Date` | tag 1004 over an RFC 3339 full-date text string `YYYY-MM-DD` |
| `Timestamp` | integer count of microseconds since the Unix epoch |
| `Duration` | integer count of microseconds |
| enum label | text string |
| struct, record, row | map with text keys, one member per present stored member; an absent member is omitted |
| tuple | array of its places in position order; an absent place is `null` |
| table with an element form | array of its elements in ascending element order (Annex B.4); a single-member element is its member's value, and a multi-member element is the array of its members in the order the element form lists them |
| relation, view result | array of its occurrences in the relation's order (Annex B.5), each its record, or its element for the rows of an element-form table |
| `Object` | the CBOR value itself, numbers as integers or tag 4 decimals |
| `Blob` | map `{ "digest": <64-byte byte string>, "media": <text>, "name": <text>, "size": <unsigned integer> }`, `name` omitted when absent |
| opaque token that the service mints (a session capability, a frontier, a commit, an occurrence) | byte string |
| `none` in a position that cannot be omitted | `null` |

A received item that decodes to no value of its type is malformed. Each of these is one:

- a map with two equal keys, a repeated `Set` element, or a repeated `Map` key;
- a floating-point number, or a simple value or a tag outside the list above;
- a tag 37 over other than 16 bytes, or a tag 1004 over an invalid date;
- a text string that is not valid UTF-8.

**Examples.** The decimal `2.5` is `4([-1, 25])`; the `Uuid` `3f2c7a1e-0b4d-4c6e-9a8f-1d2e3f405162` is `37(h'3f2c7a1e0b4d4c6e9a8f1d2e3f405162')`; the `Date` `2026-09-22` is `1004("2026-09-22")`; the instant `2026-09-22T10:00:00.250Z` is `1790071200250000`; the `Map(key: Text, value: Int)` holding `daily` with 100 is `[["daily", 100]]`; `(none, 5)` of the type `(Text?, Int)` is `[null, 5]`.

**Rows.** A row carried as one value is the map of its stored members: its fields and structs, its element-form tables, and its nested tables as arrays of their row records.

**`Object`.** An `Object` is a text-keyed map, an array, a text, a number, or a boolean, nested to any depth; a number is an integer, a bignum, or a tag 4 decimal fraction.

**`Blob`.** The canonical media type of `media` is the type and subtype in lower case, then each parameter as `;name=value`, names in lower case and sorted, values as given, with no whitespace (§15.1).

**Diagnostic notation.** Examples in this document write CBOR in the diagnostic notation of RFC 8949 §8, in `cbor-diag` blocks, with `/ … /` comments where a part is elided. They list map members in reading order.

**Limits.** The language version `liasse 0.0.1` fixes these limits for every value, wherever it is written, computed, or received:

| Value | Limit |
| --- | --- |
| `Decimal`, and a number inside an `Object` | the absolute value of the exponent of its minimal-scale fraction (A.1) MUST NOT exceed 16383 |
| `Date` | from `0000-01-01` through `9999-12-31` |
| `Timestamp` | the instants from `0000-01-01T00:00:00Z` up to, and not including, `10000-01-01T00:00:00Z` |
| `Duration` | a magnitude below 3 652 425 days, so that the difference of any two timestamps is a duration |
| `Int` | unbounded |

A received value beyond a limit is malformed. A computed value beyond a limit is an evaluation error that rejects the read or the transition that computes it. A literal beyond a limit is a build error.

### A.5 Key-eligible types

The members of a `unique`, a `Set`'s element type, and a `Map`'s key type MAY use:

```text
Text, Bool, Int, Decimal, Bytes, Uuid, Date, Timestamp, Duration,
refinements of them (such as an Enum),
and static structs composed solely of key-eligible required members
```

A type that a component publishes is key-eligible when its descriptor declares it so. The empty `Text` and the empty `Bytes` are admissible in every key-eligible position.

<a id="annex-b"></a>

---

## Annex B — Deterministic total order

This annex is normative.

Every orderable Liasse value has a deterministic ascending total order. Descending order reverses it. Sort terms compare lexicographically from left to right; identity order and then occurrence identity complete the order of every relation (B.5).

The order depends on the values alone. For two values `a` and `b` of one type, exactly one of these holds: `a` precedes `b`, `b` precedes `a`, or `a` and `b` are equal (A.1). Values of different types are ordered against each other where the language compares them: an `Int` with a `Decimal`, numerically; a refined value with its base type; and a `T` with a `T?`.

### B.1 Scalar order

| Type | Ascending order |
|---|---|
| `Bool` | `false`, then `true` |
| `Int` | mathematical integer order |
| `Decimal` | mathematical decimal order; numerically equal values compare equal (A.1) |
| `Text` | lexicographic Unicode scalar-value order, which is the bytewise order of the UTF-8 encodings; a proper prefix precedes the longer text |
| `Bytes` | lexicographic unsigned-byte order; a proper prefix precedes the longer value |
| `Uuid` | unsigned lexicographic order of its 16 bytes |
| `Date` | chronological Gregorian order |
| `Timestamp` | chronological order of the instants, as signed microsecond counts |
| `Duration` | signed length: negative durations first, then zero, then positive durations |
| an `Enum` | the order of its text, whatever the order in which the labels are declared |

Texts compare by their scalar values exactly as written: `"Zeta"` precedes `"alpha"`, and `"10"` precedes `"9"`.

### B.2 Optional values and `none`

The ascending order of `T?` is:

```text
all present T values in ascending order, then none
```

Descending reverses the complete order:

```text
none, then all present T values in descending order
```

### B.3 Object order

`Object` values use this kind rank:

```text
boolean < number < text < array < map
```

Within each rank:

| Kind | Order |
|---|---|
| boolean | `false < true` |
| number | mathematical numeric order; `1.5` and `1.50` are one number |
| text | the text order of B.1 |
| array | lexicographic element order; shorter array first after a shared prefix |
| map | members listed by key in text order, then compared member by member, each by its key and then by its value; the first member that differs decides, and a proper prefix precedes the longer list |

So `{"a": 1}` precedes `{"a": 1, "b": 0}`, which precedes `{"a": 2}`, which precedes `{"b": 0}`.

### B.4 Composite values

| Type | Order |
|---|---|
| struct, record | lexicographic members in the Unicode scalar order of their names, stored members only |
| tuple | lexicographic places in position order |
| table with an element form | the sequence of its elements in ascending element order, compared element by element; a proper prefix precedes the longer sequence, and the empty table is first; a multi-member element compares as the tuple of its members in the order the element form lists them, so a `Map` entry compares by key, then by value |
| blob descriptor | `digest`, `size`, `media`, then optional `name` |

### B.5 Rows and views

A table's default read order is its `order:` terms when it declares them, and insertion order otherwise. A view follows its declared `sort` stages, then the identity order of its rows, then occurrence identity:

- **Identity order** is insertion order for the rows of a table without an element form, fixed for each row at its insertion, the rows of concurrent transactions in an arbitrary but stable order (§4.2); ascending element order for the rows of a table with an element form, and insertion order among equal elements; for a nested row, the owner chain first, from the outermost owner inward, each owner by its own identity order, then the row's own identity order among the rows of its owner; for a row reached through a mount point, the identity order of the mount point's owner row first, then the row's identity order within the mounted instance; for a `group` output, its key ascending; and for an occurrence that a stage derives from a row, such as the output of `map` or `window`, the identity order of that row.
- **Occurrence identity** (Annex C.1) orders the remaining ties, which arise only when one row occurs several times in one relation: repeated occurrences compare their multiplicity chains element by element from the outermost, each element in the order of the relation it was read from, and a chain that is a prefix of the other comes first.

```liasse
view renewals = .subscriptions |> sort(desc(.ends_at), .price)
```

means:

1. an absent `ends_at` first;
2. present end instants latest to earliest;
3. price ascending among equal end instants;
4. identity order, then occurrence identity, as the final tiebreakers.

`desc` reverses its own term only, and the tiebreakers always ascend. Everything positional follows this order: the array of a relation (A.4), `skip` and `take`, window frames, and the positions in a live view's patch operations (§11.2). Every backend MUST produce exactly this order.

<a id="annex-c"></a>

---

## Annex C — Identity, paths, and portable encoding

This annex is normative.

### C.1 Row and instance identity

A table row has an **identity** that the backend assigns when the row is inserted, whatever inserts it. The identity is independent of the row's members: the row keeps it through every change of its members and every update that carries its table (§17.1), until it is deleted. An identity is unique among all the rows ever inserted into one table declaration of one module instance; inserting a row with equal members creates a new identity. Every portable and historical record that concerns a row names it by its identity.

Where an identity is written as text — a display-path segment, a portable record — it is written as its **identity text**: one or more characters from the unreserved set of RFC 3986 (ASCII letters, digits, `-`, `.`, `_`, `~`), other than `.` and `..` alone. Each identity has one text, fixed for the row's life and distinct from the text of every other identity of the same table declaration in the same instance, across every copy of the instance: two stores that restore one artifact and then insert rows independently MUST give distinct rows distinct identity texts. Identity text is opaque and host-facing, for the root client alone. The rows of a table with an element form have identities like every row; portable records name them by their element.

A module instance is addressed by its mount point, the root instance by `/`. It has an immutable **incarnation**, assigned when it is mounted, or, for the root, when `create` builds the application. The incarnation is kept through every update of the instance's definition and every change of its parent module's members; unmounting ends it, and mounting again at the same mount point creates a new instance with a new incarnation. An incarnation is written as identity text and is unique among the incarnations of one application and of its retained history, including instances that two stores mount independently after restoring copies of one artifact.

A view occurrence carries the identity of the row it derives from, or, for a `group` output, its group key. Each occurrence additionally has an **occurrence identity**, derived from exactly three things: the view declaration that yields the relation, or, for any other relation, the declaration that contains its expression; the source row's identity, which for a nested row includes the identities of its owners and for a row of a mounted instance that instance's incarnation; and its **multiplicity chain**, the source occurrences that created a repetition on its way into the relation, outermost first. An owner step `.^` applied to a relation adds the occurrence it starts from; a step through a view member of a row that yields rows the row does not own adds that row's occurrence; a subview adds the output occurrence that holds it. A selection, `filter`, `sort`, `skip`, `take`, `map`, and `window` keep the chain as it is, and `group` resets it. Two distinct occurrences of one relation have distinct occurrence identities, and the same occurrence has the same occurrence identity at every frontier. A client sees an occurrence identity as an opaque occurrence token (§11.2); portable records name rows by their identities.

### C.2 Paths

A **declaration path** names one member declaration of a definition's elaborated model: `/` followed by the names of the members that contain it, from the module root down, then its own name, separated by `/`, such as `/companies/offices`. The module root is `/`. A declaration inside a `public` or `role` body has the path of the scope that holds the body, followed by its own name. Migration files (LANGUAGE §14.2) and portable records (C.3, C.4) name declarations by their declaration paths.

A **display path** names one location of stored state, or one mounted instance, for the root client. It alternates declaration-name segments and row identity-text segments:

```text
/companies/k7Qx2m/offices/p2Rt8n/rooms/x4Tb9k
```

After a table without an element form, the next segment names a row by its identity text. A mount point's path is the path of its member, such as `/companies/k7Qx2m/billing`, and a path into the mounted instance's state continues after it, as in `/companies/k7Qx2m/billing/invoices/m3Pq7z`. A scalar field and a table with an element form end the path.

A root member (§4.1) is addressed by a name-only path — a bare declaration-name segment (`/flag`), and a member of a static struct at the root by its names alone (`/settings/page_size`).

A display path given to a host operation MUST begin with `/`; `/` alone is the root, and every other segment MUST be non-empty, so `//` and a trailing `/` are malformed. Segments resolve one at a time from the root against the definition of the instance that holds them, and a path that fails to resolve is rejected with a diagnostic that names the offending segment. A display path is a current address: it changes when a migration renames a declaration on it and when a hydration with fresh identities gives its rows new identity text. Display paths are host-facing, for the root client alone.

### C.3 Portable `.liasse.zip` encoding

A `.liasse.zip` artifact is a ZIP archive in the format of the PKWARE `.ZIP` Application Note with the ZIP64 extensions, holding the entries of §3.1.

- **Container.** Every reader MUST accept ZIP64 fields and records. Each entry is stored or deflated. An entry name is a safe relative path: UTF-8 text of non-empty segments separated by `/`, where no segment is `.` or `..`, the name neither begins with `/` nor contains `\`, and the first segment contains no `:`. Names compare exactly, byte for byte, and each name appears once. A name that ends with `/` is a directory marker, which readers ignore.
- **Records.** `data.cbor` and `history.cbor` each hold one CBOR data item (RFC 8949) as the entry's bytes. The keys of every record map are text strings, except the keys of a delta, which are coordinates, and the keys of a coordinate, which are unsigned integers. Instance incarnations and the identifiers of lineages, points, and transactions are identity text (C.1).
- **Typed values.** A typed value in a record is its Annex A.4 encoding, decoded under the type that the definition in effect at that point gives its position, with one difference: a table without an element form is an array of **row entries** in identity order. A row entry is the map `{ "created_at", "id", "rank", "row", "updated_at" }`: `id` is the row's identity text; `row` is the **row record**, the map of the row's stored members other than mount points, in which a nested table without an element form is again an array of row entries; `rank` is a non-negative integer giving the row's place in insertion order among the rows of its owner, the lower rank first; and `created_at` and `updated_at` hold the members of the row's `$history` (§16.12). Ranks encode the order alone: the writer chooses them, and two artifacts MAY give one row different ranks. A table with an element form is the array of its elements, as in Annex A.4.

`data.cbor` decodes to:

```cbor-diag
{
  "instance": "<instance-incarnation>",
  "selected": {"lineage": "<lineage-id>", "point": "<point-id>"},
  "state": {"tasks": [ / row entries / ]},
  "modules": {
    "<mount-path>": {"instance": "<child-incarnation>", "lineage": "<lineage-id>", "point": "<point-id>"}
  }
}
```

`instance` is the instance's incarnation, and `selected` its selected point with that point's lineage. `state` maps each stored root member to its typed value under the artifact's current definition, which is the definition of the selected point. `modules` maps the mount path (§16.5) of each child mounted at the selected point to the child's incarnation and selected point; the artifact carries that child as `modules/<incarnation>.liasse.zip`.

### C.4 Portable history records

`history.cbor` decodes to:

```cbor-diag
{
  "lineages": {
    "<lineage-id>": {"origin": "genesis", "head": "<point-id>"}
  },
  "points": {
    "<point-id>": {
      "previous": "<point-id>",
      "transactions": ["<transaction-id>"],
      "delta": {/ coordinate: change /},
      "observations": {/ observation key: observation /},
      "modules": {
        "<mount-path>": {"instance": "<child-incarnation>", "lineage": "<lineage-id>", "point": "<point-id>"}
      }
    }
  }
}
```

`lineages` holds each carried lineage: its `origin`, the text `"genesis"` or, for a lineage that continues from an earlier point, `{"lineage": …, "point": …}`; and its `head`. A lineage's points are its head and the points reached from it through `previous`, back to its origin. `points` holds each carried point:

- `previous`, the point before it, omitted at genesis, and, for the first point of a lineage that continues from an earlier point, that point;
- `transactions`, the non-empty array of the identities of the transactions whose commits the point reflects, in serial order (§16.2);
- `delta`, its change from the previous point, the genesis point's delta being its whole state from absence;
- `observations` (C.5), present when the point has recorded observations;
- `modules`, the composition point at the point (§16.3): the incarnation, lineage, and point of each child mounted there, by mount path, present for the points that select or change the children mounted at the instance.

`previous` and `origin` MAY name a point outside the carried history. Every other point that a record names is carried: in `history.cbor`, or, for a child, in the child's nested artifact. Every version that a delta names is a version of the artifact's chain (§3.1).

A **delta** is a map from a logical **coordinate** to a change `{ "after": side, "before": side }`, where a side is `{ "absent": true }` or `{ "present": <typed value> }`. A coordinate is a map with unsigned integer keys: `0` holds the **scope**, always present; `1` the row; `2` the member; `3` the element, each when the coordinate has it.

- The scope is the declaration path of the table whose row the coordinate concerns, or of the root member it concerns; for a declaration that ended within the delta, the array `["ended", path]`; and for the instance's own records, the text `"definition"` or `"modules"`.
- The row is the row's identity text. The member is the path of the stored member within the row or the root member, its names joined by `/`, with the name of each static struct on the way, such as `address/city`. The element, for an element of a table with an element form that declares a `unique`, is the element's values of that `unique` as a typed value: the value of a `Set` element, the key of a `Map` entry. Every other element-form table is recorded as one value, the array of its elements, at its member coordinate.

| Coordinate | Present while | Value |
| --- | --- | --- |
| `{0: table, 1: row}` | the row exists | the row header `{ "created_at", "rank", "updated_at" }`, plus `"owner"`, the identity text of the owning row, for a nested table |
| `{0: table, 1: row, 2: member}` | the field is present | the field's typed value |
| `{0: table, 1: row, 2: member, 3: element}` | the element exists in the table that `member` names | the element's value |
| `{0: root member}` and `{0: root member, 2: member}` | the field is present | as for a row's field, at the root |
| an optional struct or optional table, at its coordinate | it is present | `{}` |
| `{0: "definition"}` | always | the version in effect, its `meta.version` text |
| `{0: "modules", 2: mount path}` | a child is mounted there | `{ "instance": <incarnation> }` |

A row's insertion changes its row coordinate and the coordinates of every stored field, element, and nested row it holds from absent to present, and a deletion changes them back. Coordinates name declarations by their paths in the definition in effect at the delta's later point; a coordinate whose declaration ends at that point is named in the definition in effect at the earlier point, with the scope `["ended", path]`, which keeps an ended declaration apart from a later declaration that took its path. A renamed declaration keeps one coordinate through the update. In a delta, each coordinate appears at most once, and each change has two different sides. The delta of an update point holds, in its `before` sides, every value that the update took out of live state or changed.

### C.5 Recorded observations

A recorded observation is its stable path or name within the operation and its typed value.

An observation that entered stored state is carried by the point's delta, as the value at the coordinate it was written to. An observation that history must reproduce and that stays outside the state — a durable fact of the admission — is written in the point record's `observations` member: its key is the identity of the transaction that obtained it, one of the point's `transactions`, followed by the observation's stable path within that operation, and its value is the observation's typed value.

<a id="annex-d"></a>

## Annex D — Later stages

This annex is informative. It lists the features that come in later stages, each with what it will do.

- **Lifecycle hooks**: `on` and `after` events over insert, update, delete, change, and commit, the lower layer beneath `assert` and `unique`.
- **Meters, buckets, period values, temporal membership, and capacity allocation**: time-bounded row membership, recurring periods, and prioritized spending against pools, which the core writes with ordinary forms (LANGUAGE Annex D).
- **Peer composition**: siblings binding each other by interface, optional bindings, and `if #handle { … }` blocks.
- **Module-owned history retention**: a module, which owns its data, declares a retention policy for its history, and the backend discards the history that falls outside it.
- **Erasure**: a module scrubs the retained history of a row it deletes, and of the modules mounted in it, and captures the scrubbed values in an extract that the host keeps.
- **Reinsertion**: a module brings an erasure's extract back, restoring the scrubbed values in retained history and the erased rows in live state.
