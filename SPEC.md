# Liasse Specification

Specification of `liasse 0.0.1`.

Liasse is a state engine for applications. A module declares typed state, the
views that read it, and the mutations that change it. A host embeds Liasse, runs
the module as the root of an application over a store, and serves it to the
clients that it authorizes, which call its mutations and keep its views live.
Every change is a transaction over a snapshot. Transactions run in parallel, and
one waits for another only when both touch the same data. History keeps what
every past state is recomputed from.

This document specifies what the components of Liasse do: how an application is
built from modules, how its state changes, how the host and clients use it, and
what history keeps. It holds for every store, client binding, client, and host.
[LANGUAGE.md](LANGUAGE.md) specifies the source language, and
[BUILTINS.md](BUILTINS.md) the builtins.

---

## 1. Overview

### 1.1 The parts of an application

```text
host                                   the program that embeds Liasse
├── application                        a root instance and the instances mounted in its state
├── store                              state and history
├── blob storage                       storage classes over connectors
└── client binding ──── clients        connections, entry points, live views
```

- A **module** is source: a module file, its library files, and the modules
  that it requires. A package carries every version of a module and the
  migrations between them in one `.liasse.zip` file (Part I).
- The **host libraries** are the libraries that the host injects into modules
  and makes available to the clients that they serve: those that declare the
  builtins, the host types among them, and the host's own, whose opaques the
  system implements (§3.7).
- An instance is a module with state. The **application** is one root
  instance, the instances mounted in its state at any depth, and its history
  (Parts II, V, VII).
- The **host** is the program that embeds Liasse. It creates, opens, and updates
  the application, reads and writes every instance, holds its configuration, and
  gives the clients that its implementation authorizes views of the root
  instance (Part III).
- A **client** connects to the application through a client binding. It holds
  the entry points that the host gives it and that its calls return, and reads
  the views and calls the mutations that they reach, down to the instances
  mounted at any depth (Part IV).

### 1.2 A worked example

A task list where each account signs in and keeps its own tasks:

```liasse
//! A task list with accounts.
meta { name: "acme.tasks", version: "1.0.0", language: "liasse 0.0.1" }

/// What a signed-in client reaches.
template Member: {
    name: Text
    tasks: { title: Text; done: Bool; mut complete() }*
    mut add(title: Text)
    mut sign_out()
}

/// What the host gives each client that it admits.
template Guest: {
    mut sign_up(email: Text, name: Text, public_key: Bytes)
    mut challenge(): Bytes
    mut sign_in(email: Text, nonce: Bytes, session: Bytes, signature: Bytes): view Member?
    mut resume(session: Bytes, nonce: Bytes, signature: Bytes): view Member?
}

view session_life: Duration = "P30D"
view challenge_life: Duration = "PT5M"

tasks: {
    owner: Text
    title: Text { assert(size(string.trim(.)) > 0, "A title is required") }
    done = false
    mut complete() { .done = true }
}*

accounts: {
    email: Text
    set { unique(.email) }
    name: Text
    public_key: Bytes                   // the public key of the account's key pair
    view tasks = /tasks[.owner == ^.email]
    mut add(title: Text) { /tasks += { owner: .email, title } }
    mut sign_out() { /sessions -= /sessions[.account == ^.email] }
}*

challenges: {
    nonce: Bytes
    set { unique(.nonce) }
    expires_at: Timestamp
}*

sessions: {
    public_key: Bytes                   // the public key of a client's session key pair
    set { unique(.public_key) }
    account: Text
    expires_at: Timestamp
}*

/// The account that holds the session `session`, while the session lasts.
view member(session: Bytes) =
    .accounts[.email == /sessions[.public_key == session, .expires_at > now()].account]

mut sign_up(email: Text, name: Text, public_key: Bytes) {
    .accounts += { email, name, public_key }
}
mut challenge(): Bytes {
    let nonce = random.bytes(32)
    .challenges += { nonce, expires_at: now() + .challenge_life }
    return nonce
}
mut sign_in(email: Text, nonce: Bytes, session: Bytes, signature: Bytes): view Member? {
    let key = .accounts[email].public_key ?? ""     // empty for an unknown email
    assert(.challenges[nonce, .expires_at > now()] != none &&
        crypto.ed25519_verify(key, nonce + session, signature), "Sign-in failed")
    .challenges -= .challenges[nonce]
    .sessions += { public_key: session, account: email, expires_at: now() + .session_life }
    return view .member(session)
}
mut resume(session: Bytes, nonce: Bytes, signature: Bytes): view Member? {
    assert(.member(session) != none &&
        .challenges[nonce, .expires_at > now()] != none &&
        crypto.ed25519_verify(session, nonce, signature), "Session ended")
    .challenges -= .challenges[nonce]
    return view .member(session)
}
```

The host builds the application's context from its configuration, creates the
application on its first start, and gives each client that it admits a view of
the root instance through `Guest`:

```text
context                            the store, a blob connector, and the storage
                                   class default over it (§8.1)
open()                             the application that the store holds (§9.1)
create(acme.tasks.liasse.zip)      on the first start, when the store holds none
root through Guest                 the entry point of each client that the host admits (§10.1)
```

A client connects, signs in, and works on its tasks. It proves that it holds the
account's private key by signing a challenge, so its arguments carry public
keys, nonces, and signatures:

```text
client                                              host
connect                                         ->  the root instance through Guest, when the host admits the client
call challenge()                                ->  committed: the nonce
call sign_in(email, nonce, session, signature)  ->  committed: the entry point `me`
read me live                                    ->  { name, tasks: [t1: {...}, t2: {...}] }
call me / tasks / t1 / complete()               ->  me with t1 done, then committed
```

`sign_in` returns a view of the account through the shape `Member`: the
connection reaches that account's name, tasks, `add`, and `sign_out` alone. When
`committed` arrives, the live view already shows the task done. When the session
expires or is removed, the view reads `none`, and every read and call through it
is denied.

---

## 2. Conventions

### 2.1 Normative language

**MUST**, **MUST NOT**, and **MAY** are used as in BCP 14. Examples and notes
are informative.

### 2.2 The documents

| Document | Specifies |
| --- | --- |
| [LANGUAGE.md](LANGUAGE.md) | the source language: the syntax and the meaning of each form |
| [BUILTINS.md](BUILTINS.md) | the builtins: the host types, operators, templates, mixins, functions, mutations, and namespaces that the implementation of each language version provides |
| SPEC.md | the system: modules and packages, state and transactions, the host, clients, composition, blobs, evolution, and history |

---

## Part I. Modules

## 3. Modules and packages

### 3.1 Module files

```liasse
//! A task list.
meta { name: "acme.tasks", version: "1.0.0", language: "liasse 0.0.1" }

tasks: {
    title: Text { short() }
    done = false
}*
```

A module file holds the current version of one module (LANGUAGE 14.1). Its
`meta` gives:

- `name`: one or more components separated by `.`, each a lower-case ASCII
  letter followed by lower-case ASCII letters, digits, and `_`, such as
  `"acme.tasks"`;
- `version`: three base-10 integers separated by `.`, each `0` or starting
  with a digit from 1 to 9, such as `"1.10.0"`; versions are ordered by major,
  then minor, then patch, numerically;
- `language`: `"liasse <version>"`, the version of the language that the file
  is written in.

`name@version` is the module's **identity**, here `acme.tasks@1.0.0`. The name
identifies the module across its versions.

### 3.2 Library files

A **library file**, `<file>.lib.liasse`, is a library (LANGUAGE 16) beside the
module file, and each one is part of the module, which the module file's `meta`
identifies. The mixin `short` of §3.1 is in `limits.lib.liasse`:

```liasse
//! Limits of task fields.
mixin short(limit: Int = 200) { assert(size(.) <= limit, "Too long") }
```

### 3.3 Required modules

A `requires` entry names a module by its name and major version, such as
`text: "acme.text@1"` (LANGUAGE 14.3). Each required module is a **submodule**:
the package carries it as a package of its own under `submodules/` (§3.5), and
the requirement resolves to it. Its namespace holds the functions, templates,
and mixins that its `public` holds, and the requiring module applies them in its
own declarations.

### 3.4 Versions and migrations

A module evolves in versions, each complete: its module file and its library
files. Two consecutive versions whose data structures differ (LANGUAGE 15.2) are
joined by a migration file, `<from>-<to>.migration.liasse` (LANGUAGE 15). The
versions in order form the module's **migration chain**, and the lowest is the
chain's **floor**. A module's package holds the chain (§3.5).

### 3.5 Packages

A **package** is a source folder in one `.liasse.zip` file, a ZIP archive
(Annex D.1):

```text
acme.tasks.liasse.zip                  the package
  tasks.liasse                         the current version: meta { name: "acme.tasks", version: "1.2.0", … }
  limits.lib.liasse                    a library file of the current version
  submodules/
    acme.text@1.liasse.zip             an immediate submodule, as a package
  migrations/
    1.0.0/tasks.liasse                 each earlier version: its module file
    1.0.0/limits.lib.liasse            and its library files
    1.1.0/tasks.liasse
    1.1.0/limits.lib.liasse
    1.0.0-1.1.0.migration.liasse       each migration between consecutive versions
    1.1.0-1.2.0.migration.liasse
```

Its module file is the one entry at the root of the archive whose name ends in
`.liasse` and in neither `.lib.liasse` nor `.migration.liasse`. A package is
identified by the identity of its current version. Each requirement `name@major`
of each version resolves to the package under `submodules/` whose current
version has that name and that major, which carries its own submodules, and each
version elaborates from its own files and the submodules that it resolves to.

A reader reads these entries and those of a dehydrated application (§14.5), and
ignores every other. An operation that takes a package reads all of it, nested
packages included, before it changes anything, and it rejects the package,
naming the entry, when an entry name is unsafe or repeated (Annex D.1), a source
file or a nested package fails to parse, a version fails to build (§3.6), or a
requirement resolves to no submodule.

### 3.6 Building

Building a module elaborates each version: templates, mixins, slots, and
applications become the declarations that they stand for (LANGUAGE 12, 13), and
each elaborated declaration keeps its origin, the definition, the application,
and the slot that it comes from, for diagnostics and documentation. A build
checks every version and, between consecutive versions, the migration checks of
LANGUAGE 15.2 and 15.6. A build error is a diagnostic (chapter 7) that names the
file and the span, says what is wrong, and says how to fix it. An operation that
activates a version builds it, and a build error rejects the operation with the
code `build`.

### 3.7 Host libraries

The host injects **host libraries** (LANGUAGE 16) into modules. Their
declarations are opaques, which the system implements, and definitions written
in Liasse:

- the host libraries that the implementation of each language version provides,
  which declare the builtins ([BUILTINS.md](BUILTINS.md)), the host types among
  them, and whose opaques that implementation implements: the host injects them
  into every module written in that version, and they serve clients;
- the host's own libraries: any number of `.sys.liasse` files that the host
  gives in its context (§8.1). Their opaques are functions and mutations, and
  the host registers one implementation of each opaque of each library. The host
  declares what each of them serves: every module of the application and
  clients, every module alone, or the modules that one of its selectors names: a
  module name alone, a module name at one version, or a module name with a range
  of versions (§3.1). Every module includes the module of each instance and each
  submodule of one.

A host library that serves clients gives them its functions, which the views
that they write call (§10.2). Two host libraries injected into one module that
declare one name fail the build of that module's versions with the code `build`.

**Host dependencies.** The **host dependencies** of a version are the
declarations of the host's own libraries injected into its module that its names
reach, directly or through other declarations of those libraries: each opaque
with its signature, and each definition whole. Building a version records them,
the store keeps them with each version that its instances hold, and reflection
gives them (§8.3). `open()` and each activation (§9.4) check that the libraries
injected into the module of each version that they build hold each of its host
dependencies: for an opaque, an opaque with the recorded signature, and for a
definition, the recorded definition. A missing or changed dependency rejects the
operation with the code `build`.

---

## Part II. State and transactions

## 4. Instances and state

### 4.1 Instances

An **instance** is a module with state: the stored members of one version of the
module, holding values. An instance is initialized from a version (§4.2), then
mounted: the application's **root instance** at the application's root, by the
host (§9.1), and every other instance at a mount point in the state of another
instance (§11.2). A module is the same at every depth: an instance of one
version has the same members, views, and mutations at the root and at every
mount point.

Each instance has an **incarnation**, which identifies it from its
initialization to its unmounting (Annex C.2).

### 4.2 Initialization

**Initializing** a version of a module creates an instance of that version and
gives its state its first value: the initializers of its members run (LANGUAGE
7), and the initializer of each stored set inserts its **seed data**, the rows
that it starts with, as one insertion.

```liasse
labels: Text* = ["urgent", "later"]
teams: {
    name: Text
    set { unique(.name) }
    members: Text*
}* = [{ name: "core", members: ["ana"] }]
```

- Initialization is part of the transaction that mounts the instance.
- During initialization, every initializer reads the whole initial state, every
  seeded row included.
- An initializer also runs for each new owner of its member: when a row is
  inserted, for the members of the row, the seed data of its stored sets
  included; when an empty optional object takes a value, for the members that
  its record leaves out; and when an update adds the member, for every owner
  that exists (§13.4). It reads the intermediate state that the statement
  creating its owner starts from.
- A seed record supplies every required member that has no initializer.

### 4.3 Rows and identity

Each row of a stored set has an **identity**. The identity is given when the row
is inserted, kept through every change of its members and every update that
carries its stored set, and ended when the row is removed. It belongs to that
one row: a row inserted with equal members has a new identity. A row's identity
belongs to the store that holds it.

- **Order.** The rows of each owner's stored set have an **insertion order**,
  oldest first: the order of the commits that insert them, then of the
  statements of a commit. The rows that one statement inserts follow the rows
  that they come from, oldest first, then the others in the order of their
  values (Annex B).
- **Ownership.** Each owner of a stored set has its own rows: a stored set at
  the root belongs to its instance, a stored set in a row body to each row, and
  a stored set in an object's body to each owner of the object. A row belongs
  to one owner for its life.

### 4.4 Constraints and uniqueness

A body declares **constraints**, conditions on its values; the set level of a
stored set (LANGUAGE 4.5) declares **unique keys**, functions of a row whose
values no two rows of one owner share; and a program checks **assertions**,
conditions on the state that a statement reads (BUILTINS §3).

- Constraints and unique keys hold in every committed state. They are judged on
  the **complete result** of each transaction, the state that its last statement
  leaves, and an intermediate state between its statements MAY break them.
- A stored set of cardinality `+` adds to its set level the constraint that it
  holds a row.
- A complete result in which a constraint is false rejects the transaction with
  the code `constraint`.
- A complete result in which two rows of one owner give equal values of a
  unique key rejects the transaction with the code `uniqueness`, and the
  diagnostic names the stored set, the key, and the value.
- An assertion is judged once, when its statement runs, over the intermediate
  state, and a false one rejects the transaction with the code `assertion`.
- The constraints and unique keys of an input's type are judged when the
  argument is bound (LANGUAGE 10.3): a false constraint rejects the read or the
  transaction with the code `constraint`, and a violated unique key with the
  code `uniqueness`.

## 5. Views

### 5.1 Reading a view

A view is read over a snapshot (§6.6) at an instant (§6.7), and yields a set of
the cardinality of its type. A read leaves state as it is and runs alongside
every transaction. A read inside a transaction runs over the transaction's
intermediate state at the transaction's instant.

### 5.2 Live views

A **live view** is a view read again after each commit that changes what it
reads, and at each instant where the clock crosses a bound that changes its
result, as when a session of `.sessions[.expires_at > now()]` expires. Each read
is over a snapshot that includes the snapshot of the read before it. The host
holds live views, and so does a client (§10.2).

## 6. Transactions

### 6.1 A transaction

A **transaction** is one change of state, run for one **request**: a call of a
client (§10.3), a native transaction of the host (§8.2), a lifecycle operation
(chapter 9), or an update (§13.1). It runs over one snapshot (§6.6), and its
statements run in order over its **intermediate state**: the snapshot with the
writes of the statements before them, the writes of the mutations that they call
included. At its end, the transaction's complete result is judged (§4.4) and
applied at once, as one **commit**, or the transaction is rejected or cancelled
(§6.5), and state stays as it was.

One transaction spans every instance that it reaches: a mutation that calls
through a mount point, or a child's call of a mutation that its parent
provides, changes the parent and the child in one commit (§11.3, §11.4).

### 6.2 Writes

Over a stored set, the statements of LANGUAGE 10.2 do this:

- **Put.** A row that `rows[conditions] = r` changes keeps its identity.
- **Replacement.** In `rows = e`, an element of `e` that has the identity of a
  row of the stored set, as when `e` reads the stored set itself, is that row,
  and it takes the values of `e`. Any other element of `e` matches the row that
  gives its value of a unique key of the stored set, which keeps its identity
  and takes the new values. The rest of `e` is inserted, and the rows of the
  stored set left unmatched are removed. An element of `e` whose values of two
  unique keys are given by two different rows rejects the transaction.
- **Removal.** The removals of one statement are planned against the
  intermediate state before it and applied together.
- **Unchanged.** A write of a value equal to the one that it replaces leaves
  its place as it was.

A constraint relates rows by value, so it guards removals:

```liasse
projects: {
    name: Text
    set { unique(.name) }
}*
tasks: {
    project: Text
    title: Text
    assert(.project in /projects.name, "A task belongs to an existing project")
}*

mut delete_project(name: Text) {
    .tasks -= .tasks[.project == name]
    .projects -= .projects[name]
}
```

Removing a project that still has tasks rejects the transaction with the code
`constraint`; `delete_project` removes the tasks with it.

### 6.3 Mutation calls

A called mutation runs at the calling statement, over the intermediate state,
and a failure anywhere rejects the whole transaction. `path.f(args)` (LANGUAGE
10.2) runs `f` once for each element of the value at `path`, all runs in
parallel: each run reads the intermediate state before the statement and its
own writes, the writes of the runs apply together, and two runs that change or
remove one place reject the transaction with the code `evaluation`.

### 6.4 Calls

Each call of a function or a mutation (LANGUAGE 5.2) has a **class**, which
fixes where the call stands and how the host orders it. A call is of one class:

- **Immutable.** Its result depends on its arguments alone.
- **Stable.** Its result depends on its arguments, the state that it reads, and
  the instant (§6.7), as a read of the time does.
- **Drawn.** Its result comes from outside the snapshot and the arguments: a
  fresh identifier, bytes from a cryptographically secure source, a new key
  pair, a result that reads data of the host, such as a time zone database, or
  the answer of a remote service. The transaction **draws** that result, and
  each evaluation draws again.
- **Effect.** It acts on the world outside the application and the host, and
  what it does stays done: it sends a message, moves money, or calls another
  system.
- **State-changing.** It changes state as a write does: `modules.init`
  initializes the instance that the assignment around it mounts (§11.2), and
  `modules.update` gives an instance a package (§13.1).

A call is also **asynchronous** when it MAY wait on a resource outside the
process that runs the transaction, such as a key service, a remote store, or a
network service, and **synchronous** when it evaluates within that process. A
call of a function is immutable or stable, and synchronous.

**Declared and inferred.** The class of an opaque comes from the system that
implements it (§3.7):

- The class of a builtin follows from its definition in BUILTINS.md, and the
  implementation of the language version declares which builtin mutations are
  asynchronous.
- An opaque mutation of one of the host's own libraries is drawn when its
  signature has a result type, and an effect when its signature ends with its
  parameters, and it is asynchronous.
- An opaque function of one of the host's own libraries is immutable and
  synchronous, as its implementation MUST be.

The build infers the class of every other call, with the call's arguments in
place of the parameters or the inputs:

- A call of a `fn` written in Liasse is stable when the body or the defaults of
  the `fn` read state or make a stable call, and immutable when they read their
  parameters alone and make immutable calls alone.
- A call of a `mut` written in Liasse takes the widest class of
  what its program reads, writes, and calls, in the order immutable, stable,
  drawn, state-changing, where a write and an effect call count as
  state-changing, and it is asynchronous when one of its calls is.

**Where calls stand.**

- The argument of a definition, every build-time value, such as a template's
  argument or a constant, and a unique key make immutable calls.
- A view, what a returned view evaluates at each read (LANGUAGE 10.4), a body
  constraint, and a function make calls of functions.
- An initializer and the default of a mutation's input also make immutable,
  stable, and drawn calls of mutations, synchronous and asynchronous.
- A program makes calls of every class: an effect call as a statement, and a
  state-changing call as a statement or as the whole right-hand side of a
  `let`, a `return`, or an assignment.
- A migration program (LANGUAGE 15.5) makes immutable, stable, drawn, and
  state-changing calls, and each mutation that it calls, directly or through
  other mutations, calls zero effects.

A **host call** is a drawn, asynchronous, or effect call, which the host
performs for the transaction. A transaction's evaluations are a function of its
snapshot, its request, its instant, and the results of its host calls: the
language version fixes the result of every operator, of every builtin function,
and of every immutable or stable builtin mutation.

**Order.** A transaction's result is that of its statements run in order, and
the host orders its host calls by the values that they need alone:

- a host call starts once its arguments are evaluated, and the evaluations that
  are independent of its result go on while it runs;
- host calls whose arguments are independent of each other's results run
  concurrently;
- a transaction that waits on a host call makes another transaction wait only
  on the grounds of §6.6;
- a host call that fails, or that outlasts the host's bound, rejects the
  transaction with the code `host`.

**Effects.**

- A statement of a program (LANGUAGE 10.2), or a native transaction (§8.2),
  calls an effect, with values as its arguments.
- The host performs the effects of a commit once the commit is durable: the
  effects of the run that commits (§6.6).
- The store holds each effect, with its arguments, from its commit until the
  host has performed it or reported it, so a host that stops performs the
  effects that remain when it starts again.
- Each effect has an **effect identifier**: its commit's identity, with the
  effect's ordinal among the effects of that commit. The host performs an effect
  at least once: it attempts it until an attempt succeeds or the host's bound on
  attempts is reached, and each attempt carries the identifier. Each effect that
  reaches the bound on attempts is reported to the host.
- The effects of one commit, and of different commits, run in any order and
  concurrently.

### 6.5 Outcomes and results

A transaction ends in one outcome:

```text
committed   its complete result became the state, as one commit
unchanged   it calls zero effects, its complete result equals its snapshot, row identities included, and state stays as it was
rejected    a check or an evaluation failed, and state stays as it was
cancelled   its call closed before its commit (§10.3), and state stays as it was
```

A mutation's result is evaluated in the state that its transaction leaves: the
committed state, or the snapshot for `unchanged`. A returned view
(LANGUAGE 10.4) that a client receives is an entry point of its connection
(§10.1), and one that the host receives is one of its live views.

### 6.6 Concurrency

Transactions run in parallel. Each runs over a **snapshot**: the state of the
commits that were visible when it started, which it **observes**. Its commit
becomes visible at once to every transaction and read that starts after it.
Each commit has an **identity**. Commit identities are totally ordered, and a
commit's identity follows the identities of the commits that it observed.

A **place** is a row of a stored set, or a member of an instance's root that
holds a value, an object, or an instance, each with the members of its
objects. A transaction **reads** exactly what its evaluations reach:

- a path reads the places that it passes through and reaches;
- a selection reads the rows that it matches and the absence of every other row
  that would match it: `.accounts[email]` reads one key of `.accounts`, and
  `.accounts[email].tasks` reads the tasks of one account;
- judging a constraint or a unique key reads what it reads. A transaction judges
  the constraints and unique keys whose reads its changes touch, and every other
  one holds as it held in the snapshot.

A transaction waits for another, or runs again, only on one of these grounds:

- **One place.** Both change or remove one place.
- **One key.** Both give rows of one owner equal values of one unique key.
- **Cycle.** Their reads and writes order each before the other, as when each
  reads a place that the other changes.

A transaction that has a ground with another that commits runs again over a new
snapshot that includes that commit, and MAY wait for the other to end before it
does. Every other transaction ends over the snapshot that it ran on.

The commits of concurrent transactions change disjoint places, and the
committed state and every result equal those of the committed transactions
running one after another in some order: execution is serializable. An update
of an instance runs in parallel with the transactions of that instance
(§13.5).

The reads and writes of a native transaction of the host (§8.2) MAY be taken
more broadly than this section gives, such as a whole stored set for a selection
of it. A ground that these broader reads and writes alone give makes the native
transaction alone run again.

A transaction that runs again ends with the outcome of its last run. A
transaction that reaches the host's bound on runs is rejected with the code
`contention`.

Two withdrawals from one balance change one place: the second runs again over
the first's result, and fails its assertion when the remaining balance is below
the amount.

```liasse
mut withdraw(amount: Decimal) {
    assert(.balance >= amount, "Insufficient funds")
    .balance -= amount
}
```

### 6.7 Time

Each transaction and each read has one **instant**, in microseconds, fixed when
it starts, and every evaluation in it that reads the time reads that instant.
A transaction's instant is the instant of its commit (§14.1). Instants follow
the host's wall clock: a later commit MAY hold an earlier instant when the
clock moves back.

## 7. Diagnostics

Every rejection carries a **diagnostic** of four members:

- `code`, one of the codes below, each with one meaning;
- `message`, a human-readable explanation; a failing constraint or assertion
  gives its own message or the one generated from its condition;
- `path`, the display path (Annex C.4) of the state, or the declaration path
  (Annex C.3) of the declaration, at fault;
- `trace`, the path of the failure across the transaction, outermost first: the
  request, with the index of the mutation in a call's list (§10.3); each
  mutation call, host call, and migration program on the way, calls through
  mount points included; and the failing statement, constraint, or expression.
  Each frame carries its declaration path and its source span, and a frame that
  a template or a mixin brought names the applications that it was inlined
  through.

| Code | Meaning |
| --- | --- |
| `malformed` | an argument or a received value fails to parse against its declared form, a view that a client writes fails to parse, or the content of a blob argument fails to arrive or to match its descriptor (§10.2, §12.2, Annex A.4) |
| `constraint` | a constraint is false, one of an input's type included (§4.4) |
| `uniqueness` | two rows of one owner give equal values of a unique key (§4.4) |
| `assertion` | an assertion of a program is false (§4.4) |
| `evaluation` | an evaluation failed: a zero divisor, two runs of one statement that change one place, a value beyond a limit (Annex A.2), or a read of a view that a client writes past the host's bound (§10.2) |
| `build` | a version fails to build, a host dependency is missing or changed, a package disagrees with its module's migration chain, an instance fails the shape of its mount point or a `provide` block fails its instance's `Parent` member, or the calls between instances form a cycle (§3.6, §3.7, §9.4, §11.3, §13.1) |
| `host` | the host failed or refused: a host call, a blob connector, or a storage class that fails to hold or deliver content (§6.4, chapter 12) |
| `contention` | the transaction reached the host's bound on runs (§6.6), or a search for an update reached the host's bound (§13.1) |

The host receives every diagnostic whole. A client receives the code, and the
message when a module wrote it; in development mode, which the host turns on, it
receives the whole diagnostic.

---

## Part III. The host

## 8. Native access

### 8.1 The host

The **host** is the program that embeds Liasse. It builds the application's
context from its configuration:

- the store;
- the blob connectors and the storage classes over them (§12.3);
- its own host libraries, each with what it serves and one implementation of
  each of its opaques (§3.7);
- the client bindings that clients connect through (§10.1).

It then creates or opens the application (chapter 9), which the clients that it
authorizes reach through its client bindings (§10.1). Several processes MAY run
the host over one store: they share the application, and each serves its own
connections.

### 8.2 Native transactions

The host reads and writes every instance natively. It reads every member and
view, writes every stored member, calls every mutation, and initializes, mounts,
gives packages to, and unmounts instances. What a native transaction does to
state, a mutation does in its own instance and the instances mounted in it, and
what a mutation does, a native transaction does.

A **native transaction** is one transaction (chapter 6) whose statements are the
host's operations. The types, constraints, unique keys, and shapes of every
instance that it changes judge its writes, as they judge a mutation's. The host
gives a native transaction as a procedure, which Liasse runs once for each run
(§6.6).

### 8.3 Reflection

Reflection gives the host each module's migration chain and target, each
instance's version and target, with the diagnostics of a blocked target (§13.1),
the host dependencies of that version (§3.7), its members with their
contracts, its state, and the instances mounted in it, with the display path
(Annex C.4) of every row and mount point and the copy status of blob content.
Display paths address instances, rows, and mount points in native operations.

## 9. Application lifecycle

### 9.1 Create and open

`create(package)` creates the application in an empty store: it initializes the
package's current version (§4.2) and mounts the instance at the application's
root, as the application's first commit. An instance of a version that declares
a `Parent` member (§11.3) is mounted at a mount point alone.

`open()` opens the application that a store holds: it checks the host
dependencies of the version of each of its instances (§3.7), builds each
version, and checks each blocked target again (§13.1).

### 9.2 Update

`update(package)` makes the current version of the package the target of its
module (§13.1), and checks again the blocked targets of the module's instances.
`update` reports each target that it leaves blocked, with its diagnostics. An
`update` that changes no target, and moves no instance, ends as `unchanged`.

### 9.3 Export and restore

`export()` writes the application as a dehydrated application (§14.5), read at
one snapshot. `restore(artifact)` makes a dehydrated application the application
of an empty store: it keeps the history, and recomputes the state from it
(§14.2).

### 9.4 Activation

`create`, `restore`, each initialization (§11.2), and each update (§13.1)
validate everything that they activate before they activate it at once:

- each version's host dependencies hold (§3.7), it builds under its language
  version and the host libraries injected into its module, and its requirements
  resolve (§3.5, §3.6);
- each storage name resolves to a storage class (§12.3);
- each migration on the routes of an update passes its checks (§13.2), and the
  mutations that its programs call reach zero effects (§6.4);
- the resulting state satisfies every constraint and every unique key, the
  public views and mutations of each instance conform to the shape of its mount
  point, each mount point's `provide` block conforms to its instance's `Parent`
  member, and the calls between instances form an acyclic graph (§11.3).

---

## Part IV. Clients

## 10. Clients

### 10.1 Entry points

A **client** connects to the application through a **client binding**, from a
frontend, or from the host's own process, where it serves one frontend as its
proxy. Its **connection** holds its **entry points**, the views returned to it
(LANGUAGE 10.4), each until the client releases it:

- the host gives it the views of the root instance that its own implementation
  authorizes, each through a shape that the host chooses;
- each call returns the views that its mutations return (§10.3).

The connection ends when the client or the binding closes it, and its entry
points end with it.

The host holds every authority: every check runs in the host, and a client holds
what the host sent it. Row identities, incarnations, the store's own
identifiers, and the module's source stay in the host, and they reach a client
through the diagnostics of development mode alone (chapter 7).

### 10.2 Reads

An entry point **reaches** the views and the mutations that its shape lists,
and, at any depth, what each view that it reaches presents of the rows and
objects that the view yields (LANGUAGE 10.4, 11.2), and what the shape of a
mount point lists of its instance (§11.4). A client uses what its entry points
reach as a module uses an instance through the shape of its mount point: it
**reads** each view, and **calls** each mutation, that they reach, and names it
in steps from an entry point: a view with its arguments, a mount point, a
mutation, which ends the steps, or a row or an object that the view before it
yielded to the client:

```text
challenge                     a mutation of the root instance that Guest lists
me                            the entry point me
me / tasks                    the view tasks of the account that me yields
me / tasks / t1 / complete    the mutation complete of the task t1 of me / tasks
```

A read gives the view's result once, or live (§5.2), each read judging the
client's reach again over its snapshot, until the client stops it, the
connection ends, the view leaves the client's reach, or a read is rejected
(chapter 7).

A client also reads views that it writes over what its entry points reach, as a
module writes a view (LANGUAGE 8.1): their expressions read what the client
reaches and call the functions of the host libraries that serve clients (§3.7),
as `me / tasks |> filter(!.done) |> take(20)` reads at most twenty open tasks.
The host bounds the work of each read of a view that a client writes, and a read
past the bound is rejected with the code `evaluation`.

A view with parameters, and a mutation, take **arguments**, each a value of its
input's declared type, with one for each required input. An argument for an
undeclared input, a missing required input, and a value that reads as no value
of its type (Annex A.4) reject the read or the call with the code `malformed`.
The default of an omitted input is evaluated once for each read or call, and a
diagnostic omits the values of arguments.

What a read or a call names is in the client's reach over a snapshot when an
entry point reaches it there: a row reached through a view is in reach when the
view yields it over that snapshot. A read of anything but a view in reach, a
view that the client writes and that reads beyond the client's reach or calls a
function of a host library that serves no client, and a call of anything but a
mutation in reach end in the **denial**: the code `denied` and one fixed
message, whatever the reason.

### 10.3 Calls

A call names one mutation, or a list of them, which run in list order as one
transaction (chapter 6):

```text
call [ me / add(title: "A"), me / add(title: "B"), me / tasks / t1 / complete() ]
```

A call passes these steps in order, each over every mutation of a list, and the
first that fails decides its outcome, with state as it was:

1. **Resolve** each mutation that the call names (§10.2). A failure is the
   denial of the whole call.
2. **Read** the arguments. A failure rejects the call, naming the index of the
   mutation in a list.
3. **Judge** the constraints and unique keys of the inputs' types (§4.4).
4. **Receive** the content of each blob argument (§12.2).
5. **Run** the transaction, which resolves each mutation again over the snapshot
   of each run, as one of its reads (§6.6). A failure names the index of the
   failing mutation.

A call commits only while it is open: a call that its client closes, or whose
connection ends, before its commit is cancelled (§6.5). The host sends each
outcome once, to the call while it is open, and `committed` once the commit is
durable:

| Outcome | Carries |
| --- | --- |
| `committed` | for each mutation, in list order, its result (§6.5): a value, or an entry point |
| `unchanged` | for each mutation, in list order, its result |
| `rejected` | the code and the message (chapter 7), and for a list the index of the failing mutation |
| `denied` | the code `denied` and the fixed message |

When a client receives `committed` or `unchanged`, the result that it holds of
each view that it reads live through the call's connection is over a snapshot
that includes the call's snapshot and, for `committed`, its commit, and so is
every result that it receives after.

---

## Part V. Composition

## 11. Mounted modules

### 11.1 Mount points

A **mount point** is a member of the template `Module`, of cardinality one or
`?`, whose elements are instances of a module. The body of `Module` lists:

- the views and the mutations that the owner reads and calls in the instance, as
  `view name: T`, `view name(parameters): T`, and `mut name(parameters)`
  (LANGUAGE 11.1): the shape of the mount point, which the instance's public
  views and mutations (LANGUAGE 14.2) alone satisfy;
- at most one `provide { … }`: the views and the mutations that the instance
  reaches through its `Parent` member (§11.3), declared at the mount point,
  where `.` is the mount point and `^` its owner.

In this example, `data` is an optional mount point whose instances present
`home` and `set_email`, and `join` mounts a new instance of a package there:

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
    mut join(artifact: Blob) { .data = modules.init(artifact) }
    mut change_email(email: Text) { .data.set_email(email) }
}*
```

`Module(target: Text? = none)` takes as its `target` a module's `name@version`,
the target of each instance of that module that the mount point holds (§13.1).
`Module` counts its `target` in the data structure (LANGUAGE 15.2), and a
migration changes it with a `retype`.

An optional mount point starts empty. A required mount point holds an instance
in every committed state in which its owner exists: the transaction that inserts
the owner also mounts it.

### 11.2 Initializing and mounting

`modules.init(artifact)` reads a package from a blob (§12.4) and initializes its
current version (§4.2) into a new instance. A statement assigns the instance to
a mount point, which **mounts** it there. Within the transaction, the version
and the instance are validated as an activation validates them (§9.4), and a
mount point that holds an instance rejects a mount.

### 11.3 Provided context

A module declares the context that it needs from its parent with at most one
member of the template `Parent`, at its root (LANGUAGE 14.4). The body of
`Parent` lists views and mutations, as `view name: T`,
`view name(parameters): T`, and `mut name(parameters)` (LANGUAGE 11.1), which
the `provide` block of the instance's mount point alone satisfies (§11.1). The
member holds no state.

- A read through the member reads the parent's state live, at the child's
  snapshot and instant.
- A call through the member runs the provided mutation in the parent, inside the
  child's transaction, at the calling statement, over the parent's intermediate
  state.
- The mutations that call each other across instances, through mount points
  and through provided mutations, form an acyclic graph, and a cycle rejects
  the activation with the code `build`.

### 11.4 Reading and calling through a mount point

- `.m != none` holds when the mount point `.m` holds an instance.
- `.m.v` reads the member `v` that the shape lists, in the instance at `.m`, as
  a member is read through a set (LANGUAGE 6.1): `.users.data.home` reads
  through every user whose mount point holds an instance.
- `.m.f(args)` runs the child's mutation `f` that the shape lists in the
  instance at `.m`, as a call through a set runs (§6.3), inside the caller's
  transaction, over the child's intermediate state, and its result presents
  what the shape lists.
- The statements of a mutation write the state of the instance whose module
  declares it.

### 11.5 Unmounting

Assigning `none` to a mount point, removing the row that holds it or an owner of
that row, and an update that removes the mount point **unmount** its instance,
in that transaction. The instance's state leaves live state, with the instances
mounted in it, recursively, and its incarnation ends. An entry point into the
instance is out of reach from then on (§10.2).

---

## Part VI. Blobs

## 12. Blobs

A **blob** is binary content held outside state and named in state by a
descriptor. The host decides where the content lives (§12.3).

### 12.1 Descriptors

A **descriptor** is a value of four members:

```text
digest   the digest of the content
size     the number of bytes of the content
media    the media type of the content, in canonical form
name     an optional file name
```

- The implementation defines the digest function. One digest names one
  content, always the same bytes.
- The canonical form of a media type is its type and subtype in lower case,
  then each parameter as `;name=value`, the names in lower case and sorted, the
  values as given, and whitespace removed.
- A digest outside the implementation's form, a negative size, and a media type
  outside its canonical form make the descriptor malformed.

### 12.2 Uploads

A client sends a blob as an argument: its descriptor among the arguments, and
its content through its binding (§10.1). The host adds content beside a native
transaction.

- A call receives the content of each of its blob arguments from its own
  connection.
- Before the call runs, the host verifies that content against its descriptor.
  Content that fails to arrive, or to match, rejects the call with the code
  `malformed`, naming the input.

A transaction commits once the storage class of each member where its complete
result stores a descriptor holds the content in as many verified copies as the
class requires. A class that fails to hold it rejects the transaction with the
code `host`, naming the member.

### 12.3 Storage classes

A stored member that holds descriptors names where their content belongs with
its **storage name**: `default`, or the name that its type gives, as in
`Blob(storage: "archive")`. The host maps storage names to storage classes. A
**storage class** is host configuration: an ordered list of locations, each a
**connector**, which gives the host access to one storage system, with its
configuration, and the number of copies that it keeps. The host verifies every
result of a connector before it counts, records, or delivers it.

```text
storage_class(name, class)           defines the class name
bind_storage(address, class)         binds the storage name default at address to class
bind_storage(address, name, class)   binds the storage name name at address to class
```

- An address is the display path of the root, of a row, or of a mount point.
- A stored blob resolves its storage name through the binding at the nearest
  address that holds it: its own row, then each owner outward to its instance's
  root, then through the mount point into the parent, up to the application's
  root.
- Content satisfies its class when as many of the class's locations as its
  copies hold a verified copy.
- When the class that a member's content resolves to changes, through a binding,
  a change of a class, or an update that changes the storage name, the host
  moves the content in the background: state and history stay as they are, and
  reads keep working.

### 12.4 Reads

A client reads the content of each descriptor that a view in its reach shows
whole (§10.2), and verifies it against the descriptor before it uses it.

Initializing an instance, giving it a package, and updating it read the verified
content of the package: first the content that their transaction received for an
argument with the same descriptor, then the class's locations. When every source
fails to deliver it, the transaction is rejected with the code `host`.

### 12.5 Retention

Content is kept while live state or history reaches it. Each descriptor in live
state keeps the copies that its class requires. Content that history alone
reaches, such as a package or a removed descriptor, keeps at least one verified
copy.

---

## Part VII. Evolution and history

## 13. Migrations and updates

### 13.1 Targets

The application holds each module's migration chain (§3.4): the versions and the
migrations of every package that it is given for the module's name. Each package
given for a name agrees with that chain: for each version and each migration
that both hold, it carries the same files. A package that disagrees rejects the
operation that gives it, with the code `build`, and a module that diverges takes
a new name, which starts a new chain.

Each module has a **target**: the current version of the package that `update`
last gave for it (§9.2). `modules.update(m, artifact)` gives the instance at
the mount point `m` the package that `artifact` holds (§12.4). An instance's
target is the `target` of its mount point's `Module` type (§11.1), else the
current version of the package that `modules.update` last gave it, else its
module's target, else its active version. Giving an instance a package of
another module, or giving a package through `modules.update` to an empty mount
point or to one whose type has a `target`, rejects the operation that gives it.

An **update** moves instances toward their targets, as one commit of its own,
validated as an activation (§9.4). It passes when the checks of its activation
pass with its instances at the versions that it moves them to and every other
instance at its active version, and each of its instances is needed: leaving any
one of them at its active version fails a check. An instance whose active
version differs from its target moves as soon as an update that holds it passes:
to its target, or as far along its route toward the target as an update passes.

- A **joint update** moves several instances: a parent whose target lists in a
  `Module` body a member that only its child's target adds, and that child,
  whose target lists in its `Parent` body a member that only the parent's target
  provides, move in one.
- Updates run in parallel. When the checks of one read an instance that another
  moves, the update that holds the instance nearer the root moves first.
- An instance short of its target runs under the version that it reached, and
  its target is **blocked**. Reflection gives each blocked target with the
  diagnostics of the checks that fail it (§8.3), and the diagnostic of a check
  between two instances names both and the member at fault.
- A blocked target is checked again at each commit that changes what its failing
  checks read, or the target of an instance that they read, at `open()` (§9.1),
  and when `update` gives its module's target again. The host bounds the search
  for an update that holds it, and a search that reaches the bound leaves the
  target blocked with the code `contention`.
- An instance runs its active version as the package that activated it
  elaborates it (§3.5).

### 13.2 The migration chain

An update moves each of its instances along its module's migration chain (§3.4),
up or down. The build checks of §3.6 hold for every migration on the routes of
an update, against the versions that each route uses, before the update
activates; a failure rejects the update and names the path.

### 13.3 Routes

The route of an instance in an update runs along its module's migration chain,
from its active version `c` to the version `t` that the update moves it to, and
it takes each version and each migration from the package, among those given for
the module, that the application was given last:

- For an upgrade, `c` below `t`, the route crosses each consecutive pair from
  `c` up to `t`, applying each migration's `up` direction. For a downgrade, `c`
  above `t`, it crosses each pair from `c` down to `t`, applying each
  migration's `down` direction.
- A route exists when the data structure of the instance's active version equals
  that of the chain's version `c`. Otherwise the update is rejected, naming the
  instance's version.

### 13.4 Applying a step

One step applies one migration in one direction, or, between consecutive
versions with equal data structures, carries the state as it is to the next
version. A step `down` applies the inverse of each entry: an `add` removes what
it names, a `remove` restores what it restates, a `rename` renames back, and a
`retype` maps through its `down`. Within a step:

1. **Structure.** A renamed declaration continues the previous one: its rows
   keep their identities, their values, and their row histories (§14.3), and a
   renamed mount point keeps its instance and its incarnation. A removed
   member's values and a removed stored set's rows leave live state. A member of
   unchanged type keeps its values; a value changes type only through a
   `retype`, whose mapping gives the new value at each place, with `old` the
   previous value. A mapping that fails, or yields no value of its `to` type,
   rejects the update, naming the path and the place.
2. **Values that enter.** An added stored set starts empty. At each place that
   enters live state, an added member takes its initializer, or `none` when its
   cardinality allows zero, and the program writes every other member.
3. **Programs.** The direction's programs run over the result in the order of
   the file, a scope's programs at the scope's position, each over the
   intermediate state that the programs before it leave, with `.` and `old`
   bound as LANGUAGE 15.5 states.

The steps of a route apply in order, each reading the result of the step before
it as `old`. In a joint update, the programs of each member's last step run
after the routes of the members mounted in it, at any depth, and those routes
run after the rest of its route. The results of the routes of an update are
judged and committed at once.

### 13.5 Updates in parallel

An update runs in parallel with the transactions of its instances. A transaction
that did not observe the update runs under the versions that it observed and
serializes before the update, and a transaction that observed it runs under the
new versions. The parts of a route divide by what they read:

- **Per-owner parts** read and write one place at a time: the structure of each
  step, the values that enter at each place from initializers that read only
  that place, each `retype` mapping, which reads the value that it maps, and
  each program in the scope of a stored set or an object whose reads and writes
  stay in one row or one owner's object, through `.` and `old` (LANGUAGE 15.5).
  When a transaction that did not observe the update commits after it, the
  per-owner parts map that transaction's changes, as they map every other place,
  and the constraints and unique keys of the new versions judge the mapped
  changes; a mapped change that breaks one makes its transaction run again under
  the new versions.
- **File-level parts** read the previous root through `old`, or read beyond one
  place: the programs at the level of the file, the other programs of scopes,
  and the initializers that read other places. Their reads and writes are the
  update's (§6.6).

Two updates that move one instance change one place, the instance's version.
Reads run alongside an update, each under the versions of its snapshot.

## 14. History

### 14.1 What history holds

History holds the **changes** of each commit that changes state, whose complete
result differs from its snapshot. For each such commit, it holds its identity,
its instant (§6.6, §6.7), and its changes, in the order in which the commit
applies them:

- each row that it inserts, with its values, and each row that it removes;
- each value that it writes;
- each instance that it initializes and mounts, with the package whose current
  version it initializes, and each instance that it unmounts;
- each package that it gives, for a module or to an instance (§13.1);
- for an update, each instance that it moves, with the version that it moves it
  to, before the changes that its route makes.

The commit of `issue_token` holds one change, the row that it inserts, with the
digest and the account; the token reaches its caller in the reply alone:

```liasse
tokens: {
    digest: Bytes
    set { unique(.digest) }
    account: Text
}*

mut issue_token(account: Text): Bytes {
    let token = random.bytes(32)
    .tokens += { digest: sha.sha256(token), account }
    return token
}
```

The packages and the blob content that changes name are kept as blobs (§12.5).
History keeps the changes of every commit for the life of the application, and a
read of an old state MAY wait while the store retrieves it.

### 14.2 Recomputing a state

The state of the commits of a snapshot is their changes applied in the order of
their identities, each change to the state that the changes before it leave. A
move carries its instance's state along its route as the structure of each step
carries it (§13.4). The recomputed state is the state that the application
committed: the same rows, in the same insertion order, with the same values and
row histories.

The **state at an instant**, among the commits of a snapshot, is the state of
those commits before the first of them, in the order of their identities, whose
instant is later.

### 14.3 Row history

Every stored row has a **row history** of two instants:

- its **creation** is the instant of the commit that inserted the row;
- its **last change** is the instant of the last commit that changed the row's
  own stored members, the members of its objects included.

A change inside a nested stored set of the row, a write of an equal value, and
the per-owner parts of an update (§13.5) leave both as they are.

### 14.4 Past states in programs

A program loads **past rows** of a source at an instant:

- The source is a stored set of the calling instance, or a selection of its
  rows.
- The load reads the state at the instant among the commits of its
  transaction's snapshot, carried to the instance's active version as an update
  carries a state (§13.4), and yields the rows of the source there: for a
  stored set, every row that it held, the rows removed since included; for a
  selection, the rows that it selects, by identity.
- Each past row keeps the row's identity and holds its stored members and its
  row history then. The load yields the rows whose carried values hold every
  required member.
- The past rows are values.

A mutation that rewinds the task list to an instant:

```liasse
mut rewind_tasks(at: Timestamp) { .tasks = history.at(.tasks, at).row }
```

`rewind_tasks` makes the task list what it was at `at`: each task that still
exists keeps its identity and takes its past values, each task removed since is
inserted again with a new identity, and each task added since is removed.

### 14.5 Dehydrated applications

`export()` writes the application as a **dehydrated application**: a package of
the root instance's current version (§3.5) with two more kinds of entries:

| Entry | Holds |
| --- | --- |
| `history.<encoding>` | the history of the commits of one snapshot (Annex D.2), in the encoding that the extension names |
| `blobs/<digest>` | the content of each blob that the history reaches, packages included, named by its digest in lower-case hexadecimal |

The application that `restore(artifact)` makes (§9.3) equals the exported
application at its snapshot. The effects that the host has yet to perform (§6.4)
belong to the store that holds them, and a restored application starts with zero
of them.

Reading a dehydrated application rejects it, naming the entry, when its package
fails to read (§3.5), a blob entry's content has a digest that differs from its
name, or its history names a blob or a package absent from it.

---

## Annex A. Values

### A.1 Elements

A value is a set of elements, of a cardinality (LANGUAGE 4.2), each a value of a
host type (§3.7), a record, a tuple, or a set (LANGUAGE 6.4). A record, an
object, and a row hold their members by name; a tuple holds its places in order;
a stored set holds its rows. `none`, the empty set, is a value of each
cardinality that allows zero. The host libraries that declare the host types
give the values of each (BUILTINS §1), their order (Annex B), and their limits
(A.2), and they name the key-eligible types among them (A.3).

### A.2 Limits

A received value beyond a limit is malformed, a computed one rejects the read or
the transaction with the code `evaluation`, and a literal beyond a limit is a
build error.

### A.3 Key-eligible types

A **key-eligible** type is a host type that its library names key-eligible, a
refinement of a key-eligible type, or an object whose members are all required
and key-eligible.

### A.4 Encodings

An **encoding** writes values as bytes and reads them back. The records of a
dehydrated application (Annex D) and the messages of each client binding (§10.1)
are in an encoding, which each names. An encoding of Liasse values:

- writes every value of every type, and reads each written value back as the
  same value under its type;
- tells `none` apart from every other value;
- writes a set as its elements;
- reads a received value under its expected type, and a received value that
  reads as no value of that type, such as a record with a repeated member name,
  is malformed.

---

## Annex B. Order

The values of each type have one total order, which depends on the values alone.
The host libraries give the order of each host type (§3.7), and this annex the
order of sets, records, objects, and tuples. Values of different types compare
where the language compares them: a refined value with its base type, and two
sets of one element type, whatever their cardinalities.

### B.1 Sets

Two sets compare by their elements, each set's taken in ascending order, pair
by pair: the first pair that differs decides, and a set whose elements run out
first comes before the other, as `none` comes before every other set.

### B.2 Records and tuples

| Value | Order |
| --- | --- |
| record, object | lexicographic over its members, in the ASCII order of their names |
| tuple | lexicographic over its places, in position order |

---

## Annex C. Identity and paths

### C.1 Row identity

Where a row's identity (§4.3) is written as text, in a display path, it is its
**identity text**: one or more characters of the unreserved set of RFC 3986,
ASCII letters, digits, `-`, `.`, `_`, and `~`, other than `.` and `..` alone. An
identity text is opaque, host-facing, and distinct from the text of every other
identity of the same stored set declaration in the same instance.

### C.2 Instance identity

An instance's incarnation (§4.1) belongs to the store, as a row's identity does.
It is written as identity text, and it is unique among the incarnations that the
application has held.

### C.3 Declaration paths

A **declaration path** names one member declaration of a version, after
elaboration: `/`, then the names of the declarations that hold it, from the
root, then its own name, separated by `/`, such as `/companies/offices`. The
root is `/`, and a declaration of `public` has the path of the root's members.
Migration files and history records name declarations by their declaration
paths.

### C.4 Display paths

A **display path** names one place of stored state, or one mount point, for the
host. It alternates the names of declarations and the identity texts of rows:

```text
/companies/k7Qx2m/offices/p2Rt8n
/companies/k7Qx2m/billing/invoices/m3Pq7z
```

- After a stored set, the next segment names one of its rows by its identity
  text. A mount point's path is its member's path, such as
  `/companies/k7Qx2m/billing`, and a path into its instance continues after it.
- A member of the root, or of an object at the root, is named by its names
  alone, such as `/settings/page_size`. A member ends the path.
- A display path that a host operation receives starts with `/`, and every
  segment after it is non-empty: `//` and a trailing `/` are malformed. Its
  segments resolve one at a time from the root, and a path that fails to
  resolve is rejected with a diagnostic that names the segment.

### C.5 Position paths

A **position path** names one row, one member, or one mount point in a history
record. It is written as a display path, with each row named by its
**position**: its index among its owner's rows of the stored set, counted from 0
in insertion order (§4.3), in the state that the change applies to:

```text
/companies/4/offices/0
/companies/4/billing/invoices/12
```

---

## Annex D. Artifact records

### D.1 Container

A `.liasse.zip` file is a ZIP archive, in the format of the PKWARE ZIP
Application Note with the ZIP64 extensions, which every reader accepts. Each
entry is stored or deflated.

- An entry name is a safe relative path: UTF-8 text of non-empty segments
  separated by `/`, where no segment is `.` or `..`, the name starts with no `/`
  and holds no `\`, and the first segment holds no `:`. Names compare byte for
  byte, and each appears once. A name that ends with `/` marks a directory,
  which readers skip.
- The extension of a record entry names its encoding (Annex A.4).

### D.2 History record

The history record of a dehydrated application holds:

```text
history
  commits         in the order of their identities, each commit that changes state:
    id            the commit's identity, as identity text
    instant       the commit's instant
    changes       in order, each one of:
      insert      the position path of a stored set, and the row that it inserts
      remove      the position path of a row
      write       the position path of a member, and its value
      mount       the position path of a mount point, or / for the root instance, and the
                  digest of the package whose current version it initializes
      unmount     the position path of a mount point
      give        the position path of a mount point, or the name of a module, and the
                  digest of the package that it gives
      move        the position path of an instance, and the version that it moves it to
```

Values are typed values under the version of the instance that holds them, in
the record's encoding.
