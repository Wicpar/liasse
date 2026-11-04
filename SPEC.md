# Liasse Specification

This specification and [LANGUAGE.md](LANGUAGE.md) define `liasse 0.0.1`.
LANGUAGE defines the syntax of modules and what each form means. This
specification defines what every implementation shares beyond it: the
builtins, the guarantees of transactions, and the artifact that carries
instances, with their data and history, from one implementation to another.
An implementation of `liasse 0.0.1` implements both documents. Its storage,
execution, clients, and the interface of its host operations are its own.

**MUST**, **MUST NOT**, and **MAY** have their
[BCP 14](https://www.rfc-editor.org/rfc/rfc8174.html) meanings. Encoded examples
use the diagnostic notation of
[RFC 8949](https://www.rfc-editor.org/rfc/rfc8949.html) §8.

## 1. Instances and transactions

### 1.1 Instances

An instance (LANGUAGE 1) has a UUID, which identifies it wherever its state
goes. An **application** is a root instance and the instances mounted in it,
at any depth. A stored `Module` value (LANGUAGE 7.3) is a **module binding**:
it names an instance by its module name and UUID, and its place in its owner's
data is the mount point. Each instance has its own **active version** of its
module.

This specification follows one application. Its root runs version `1.0.0` of
`acme.invoice`, with the host declarations `mut ticket(): Text` and
`mut mail.send(to: Text, subject: Text, body: Text)` of LANGUAGE 7.4:

```liasse
meta { name: "acme.invoice", version: "1.0.0", language: "liasse 0.0.1" }
lines: {
    label: Text
    amount: I64
    pub mut void() {
        ^.net -= .amount;
        .amount = 0;
    }
}*
net: I64 = 0
tax: I64 = 0
total: I64 = 0
settlements: I64 = 0
number: Text?
vat: Module {
    view quoted: I64,
    mut quote(value: I64),
    provide {
        pub mut settled() { ^.settlements += 1; }
    }
}?
pub mut add(label: Text, amount: I64) {
    .net += amount;
    return view .lines += { label, amount };
}
pub mut settle() {
    .vat.quote(.net);
    .tax = .vat.quoted ?? 0;
    .total = .net + .tax;
}
pub mut issue() {
    let code = ticket();
    .number = code;
    mail.send(to: "ap@acme.example", subject: "Invoice", body: code);
}
pub mut remind() {
    mail.send(to: "ap@acme.example", subject: "Reminder",
              body: .number ?? "draft");
}
```

`add` inserts a line and returns a live view of it; a line's `void` zeroes its
amount and lowers `net`. `settle` obtains the tax from the instance at `vat`,
`issue` draws an invoice number and mails it, and `remind` only mails. The
instance at `vat` runs version `1.0.0` of `acme.tax`, and reaches the root's
provided `settled` through its `parent` member:

```liasse
meta { name: "acme.tax", version: "1.0.0", language: "liasse 0.0.1" }
parent: Parent { mut settled() }
rate: I64 = 20
amount: I64 = 0
tax: I64 = 0
pub view quoted = .tax
pub mut quote(value: I64) {
    .amount = value;
    .tax = .amount * .rate / 100;
    .parent.settled();
}
```

The instances of an application form a tree: each instance other than the root
is bound at exactly one mount point. Assigning a binding to a mount point mounts
its instance there. Assigning another binding or `none`, or removing the mount
point or its owner, unmounts the former instance, with the instances mounted in
it, in the same commit; assigning the same binding keeps it mounted. Assigning
`none` to the root's `vat` unmounts the tax instance.

### 1.2 Transactions

A **transaction** applies one builder (LANGUAGE 5.1) to committed state. Its
**participants** are the instances that it constructs, migrates, writes, or
calls mutations of. A successful transaction is a **commit**, which publishes
together, in all its participants, their changes, their history records
(chapter 5), and the effects that the transaction schedules. A failed
transaction publishes nothing.

Calling `settle()` on the root is one transaction with both instances as
participants: the root calls `quote` in the child, the child stores the amount
and its tax and calls `settled` back in the root, and the root reads the
child's `quoted` and stores its own tax and total. A failure anywhere leaves
both instances unchanged.

Implementations MUST provide these guarantees:

- Commits have one **commit order**. The committed state, each result, and each
  scheduled effect are those that applying the builders one at a time, in
  commit order, gives. Within a builder, each statement starts from the state
  that the statements before it leave (LANGUAGE 5.1).
- Each commit takes effect at one moment before it is acknowledged, in commit
  order, and each read at one moment while it runs: a read observes exactly the
  commits that took effect before its moment.
- A **live view**, a view that a reader keeps open, such as a returned view
  (LANGUAGE 5.4), tracks the commits that change it: each result is a read, at
  a later moment than the one before, and each such commit leads to a result
  that includes it.
- An acknowledged commit is durable.
- Preparing a transaction, including waiting for its host calls, delays no read.
- A transaction can run several times before it commits. Drawn calls
  (LANGUAGE 4.4) draw anew in each run, and only the committing run's results,
  records, and effects are published.

An effect call (LANGUAGE 4.4) schedules an **effect**. The commit publishes it,
also when the transaction changes no state, and the host performs it once the
commit is durable. A published effect stays pending across restarts until the
host delivers it or abandons it. Each effect has an identifier, the same at
every delivery attempt and distinct from every other effect's. The host chooses
the order, retries, and abandonment of deliveries.

### 1.3 Versions

A **transition** joins two versions of a module. A migration (LANGUAGE 8)
describes it; two versions that are consecutive in version order (LANGUAGE 7.1)
and have equal stored structure also have one without a migration, which carries
the state over unchanged. An instance moves between two versions along its
**route**, the path of fewest transitions between them, each taken in either
direction as a **step**. A route commits in one transaction, which holds only
its steps and the calls that they make: an intermediate version is never a
committed state.

The running root later moves to `1.2.0` through `1.1.0`. Version `1.1.0`
renames each line's `label` to `title`, and `1.2.0` adds `currency`:

```liasse
meta { name: "acme.invoice", from: "1.0.0", to: "1.1.0" }
in /lines {
    rename label to title
}
```

```liasse
meta { name: "acme.invoice", from: "1.1.0", to: "1.2.0" }
add { currency: Text = "EUR" }
down { drop currency; }
```

A move commits only when the interfaces of LANGUAGE 7.3 hold between each moved
instance, its parent, and its children, so instances whose new versions require
each other move together, in one transaction: reads and other transactions see
them only at their start and end states, and in between their constraints and
interfaces may break. The move bubbles like an event: each instance enters its
new version before the instances mounted in it, and finishes after them. It
enters by taking its steps up to the programs of its last step, and finishes by
running those programs. Its last programs therefore see the instances mounted
in it at their new versions, and these see its new interface.

The host chooses when an instance moves and to which version, its **target**,
unless its mount point pins one (LANGUAGE 7.3). The instance moves to the
furthest version of its route to the target at which the move commits, and
stays at its version when there is none. After each commit that changes what
stops it short of its target, it moves again as far as it can, with no further
request.

## 2. Builtins

The builtins of `liasse 0.0.1` (LANGUAGE 7.4) are the types `Bool`, `I64`,
`U64`, and `Text` of §2.1, the declarations of §2.2, the type `Blob` of §2.3,
and the mixins of §2.4. Every implementation supplies them to every module. Host
libraries supply further types and declarations (chapter 6).

### 2.1 Types

| Type | Values | Order |
| --- | --- | --- |
| `Bool` | `false` and `true` | `false`, then `true` |
| `I64` | the integers from -2^63 to 2^63 - 1 | numeric |
| `U64` | the integers from 0 to 2^64 - 1 | numeric |
| `Text` | the sequences of Unicode scalar values: U+0000 to U+D7FF and U+E000 to U+10FFFF | lexicographic by scalar value, a proper prefix first |

A text keeps the scalar values as written: a precomposed character and its
combining sequence are two texts. Texts order as their UTF-8 encodings do,
bytewise.

### 2.2 Declarations

| Declaration | Result |
| --- | --- |
| `fn N + N: N`, `fn N - N: N`, `fn N * N: N`, for `N` each of `I64` and `U64` | The sum, difference, and product. |
| `fn N / N: N`, for `N` each of `I64` and `U64` | The quotient, truncated toward zero: `-7 / 2` is `-3`. |
| `fn N % N: N`, for `N` each of `I64` and `U64` | The remainder `a - (a / b) * b`: `-7 % 2` is `-1`. |
| `fn -I64: I64` | The negation. |
| `fn !Bool: Bool`, `fn Bool && Bool: Bool`, `fn Bool \|\| Bool: Bool` | The Boolean operations of LANGUAGE 3.2. |
| `fn Text + Text: Text` | The concatenation. |
| `fn T < T: Bool`, `fn T <= T: Bool`, `fn T > T: Bool`, `fn T >= T: Bool`, for each type `T` of §2.1 | The order of §2.1. |
| `fn<V> V == V: Bool`, `fn<V> V != V: Bool`, `fn<V> V in V*: Bool` | Equality and membership (LANGUAGE 3.2). |

A result outside its type, or a zero divisor, fails the evaluation: `2 - 3`
fails as a `U64`.

### 2.3 Content

A `Blob` value references a byte sequence, its **content**, by the content's
SHA-512 digest and its size in bytes; two blobs are equal when their digests
and sizes are. The state holds the reference alone. Content travels apart from
the state and from the reads and calls that carry its reference: an
implementation lets clients write and read content by its reference.

- A commit that stores a blob makes its content durable with the commit, and
  the content stays available while state or history references it.
- An implementation MUST verify content against its reference before it
  accepts it.
- In the type `Blob(storage: Text = "default")`, `storage` is a name that the
  implementation binds to a store for the content; values and their equality
  ignore it.

### 2.4 Constraints

| Declaration | Meaning |
| --- | --- |
| `mixin assert(condition: Bool, message: Text? = none)` | `condition` holds: a constraint in a body, an assertion in a program (LANGUAGE 2.5). `message` describes a failure. |
| `mixin unique<V, K>(key: V -> K = .)` | In a set body, no two elements of the set have equal keys: no two rows of one owner, or no two elements of one argument. |

The `key` callback reads its element's data alone, makes immutable calls
(LANGUAGE 4.4), and gives a value of cardinality one or `?`. A missing key, or
a record key with a missing field, equals no other key.

## 3. Artifacts

### 3.1 Packages

An **artifact** is a ZIP archive that holds one module's **package**: the
sources of the versions that it carries, the migrations between them, the
packages of further modules, and instances. The running application's artifact
is:

```text
acme.invoice.liasse.zip
  invoice.liasse                          version 1.2.0
  migrations/
    1.0.0/invoice.liasse
    1.1.0/invoice.liasse
    1.0.0-1.1.0.migration.liasse
    1.1.0-1.2.0.migration.liasse
  instances/
    00112233-4455-4677-8899-aabbccddeeff/
      data.cbor                           the root, at version 1.0.0
      history.cbor
  submodules/
    acme.tax.liasse.zip
      tax.liasse                          version 1.0.0
      instances/
        66778899-aabb-4ccd-8eef-001122334455/
          data.cbor
          history.cbor
```

ZIP entries are unencrypted, stored or deflated; readers MUST accept ZIP64.
Entry paths are unique and relative, with `/` separators and no backslash,
empty segment, `.`, or `..`; a directory entry MAY end with `/`. Source files
are UTF-8.

- A version's sources are its library files, ending in `.lib.liasse`, and one
  other `.liasse` file, its module file (LANGUAGE 7.1). The package root holds
  the current version's sources, and `migrations/<version>/` holds another
  carried version's.
- `migrations/<from>-<to>.migration.liasse` holds the migration of each
  transition between carried versions that has one.
- `submodules/` holds one package, `<module>.liasse.zip`, for each further
  module that the artifact needs: the modules of its instances, and the exact
  versions that carried sources name in `requires`, transitively. These
  packages hold no `submodules/`.
- Module names, versions, and UUIDs in paths agree with the files' metadata.

Packages of one module MUST agree on the files of every version and transition
that they share. Readers MUST reject a package in which two versions have two
routes of fewest steps. A package MAY add a version between two others, such as
a fix to an earlier version, with the migrations that join it to them.

### 3.2 Instances

A folder `instances/<uuid>/` holds an instance of its package's module, the UUID
written in lowercase `8-4-4-4-12` form. A UUID names one folder of the artifact.
The folder holds `data.cbor` (chapter 4), `history.cbor` (chapter 5), or both:
data gives the instance's state and active version, history reconstructs them,
and with both files they agree. A module binding resolves its module name to a
package of the artifact and its UUID to a folder of that package.

A folder holds, as `blobs/<digest>` with the digest in lowercase hexadecimal,
the content (§2.3) of each blob that its data or history references, including
content that another folder also holds.

Readers MUST reject an artifact with a malformed entry, source, or file; a path
that disagrees with metadata; a missing or conflicting definition; or missing
content, or content that differs from its digest. They MAY ignore entries that
this chapter does not define.

### 3.3 Snapshots, extraction, and import

A **snapshot** is an artifact of selected instances and the instances mounted
in them, read at one committed state.

An **extraction** is a snapshot that carries each instance's history, and whose
instances leave their application. Their removal commits only once the artifact
is durable, and no commit changes them between the snapshot and the removal.
Each parent that bound one records the removal under an `extract` entry
(§5.2): extracting the running child adds to the root a record with the trace
`[{"extract": f}]`, where `f` is the artifact's fingerprint, and the changes
`[{"/": {"vat": null}}]`. The extracted histories end with the snapshot.

**Import** activates the instances of an artifact in an application, in one
commit. It selects root instances, follows their bindings, and takes each
instance's state from its data or by replaying its history (§5.5). Each root
becomes the application's root, or is bound at a mount point whose owner records
the binding under an `import` entry. An instance imported with its history
keeps it, and its later records follow the carried ones. An instance that import
loads from data starts a new history, whose first record constructs the loaded
state under an `init` entry.

An instance UUID is expected to be live in one application at most: a snapshot
is imported to recover instances that are no longer live, and import MUST
reject an instance whose UUID is live in the destination.

Import validates its whole result before it commits: sources, values,
histories, content against its digests, the tree of §1.1, and the constraints
and interfaces of every instance.

## 4. Data

### 4.1 Values

| Value | CBOR representation |
| --- | --- |
| `Bool` | `false` or `true` |
| `I64`, `U64` | an integer of major type 0 or 1 |
| `Text` | a text string |
| `Blob` | `{"digest": d, "size": n}`: `d` the 64-byte digest as a byte string, `n` the size as a `U64` |
| An object or record | a map with a text key for each stored member |
| A set | an array of its elements, in order |
| A missing optional | `null` |
| A present optional | its value |
| A module binding | `{"module": module name, "instance": UUID}` |
| A host type's value | `{"value": v}`, with `v` the representation that its host library defines |

A UUID is tag 37 over its 16 bytes in RFC 9562 order. Readers accept every CBOR
encoding of a value: any integer width, definite or indefinite length, and
map-key order. Duplicate map keys, invalid UTF-8, floating-point numbers, other
tags and simple values, missing or extra members, values outside their
declared types, and bytes after a file's map are malformed.

### 4.2 The data file

The file `data.cbor` holds one CBOR map with exactly `meta` and `data`. Its
`meta` has exactly `module`, the module name; `uuid`, the instance UUID; and
`version`, the active version, which the package carries. Its `data` holds the
stored root object at that version: every stored member of its elaborated
schema, including those that templates and mixins supply. Loading a data file
gives each row a new lifetime.

After two `Paper` lines of `40` were added, the second voided, the invoice
settled, issued, and reminded, the root's data file is:

```cbor-diag
{
  "meta": {
    "module": "acme.invoice",
    "uuid": 37(h'00112233445546778899aabbccddeeff'),
    "version": "1.0.0"
  },
  "data": {
    "lines": [
      {"label": "Paper", "amount": 40},
      {"label": "Paper", "amount": 0}
    ],
    "net": 40,
    "tax": 8,
    "total": 48,
    "settlements": 1,
    "number": "INV-7",
    "vat": {
      "module": "acme.tax",
      "instance": 37(h'66778899aabb4ccd8eef001122334455')
    }
  }
}
```

Before `issue`, `number` was `null`. The child's data file is:

```cbor-diag
{
  "meta": {
    "module": "acme.tax",
    "uuid": 37(h'66778899aabb4ccd8eef001122334455'),
    "version": "1.0.0"
  },
  "data": {"rate": 20, "amount": 40, "tax": 8}
}
```

## 5. History

An instance's history serves two purposes: its trace tells which calls ran in
each commit, and its changes reconstruct every committed state.

### 5.1 Records

The file `history.cbor` holds one CBOR map with exactly `meta` and `records`,
which Annex A specifies. Its `meta` has exactly `module` and `uuid`, as in
`data.cbor`. Its `records` is the nonempty array of the instance's **records**,
in commit order: one for each commit that the instance took part in (§1.2),
whether or not the commit changed its state. A record has these members:

| Member | Value |
| --- | --- |
| `commit` | the commit's UUID, when the commit has several participants |
| `instant` | when the commit applied, as signed integer microseconds since `1970-01-01T00:00:00Z` |
| `trace` | the calls that ran in the instance (§5.2) |
| `changes` | the changes to the instance's stored state (§5.3), absent when the commit changed none |

The records of one commit share its `instant` and, when present, its `commit`.
The histories of several instances admit one commit order together. Instants
follow the implementation's clock, and equal or decreasing instants keep their
records' order.

### 5.2 Trace

The trace lists the calls that ran in the instance. Each call is an **entry**: a
map with exactly one of the fields below, which names its kind, and `calls`, the
entries of the calls that it made, in the order in which they ran, when it made
any.

| Field | Value | The entry stands for |
| --- | --- | --- |
| `init` | a carried version | the instance's construction at that version |
| `migrate` | a carried version | the step of a move (§1.3) that enters that version |
| `mut` | a mutation path | a call of the instance's mutation from outside its code |
| `call` | a callee | a mutation call that the instance's code makes |
| `host` | a name | a host operation that changes the instance |
| `extract` | an artifact fingerprint | the removal of extracted instances (§3.3) |
| `import` | an artifact fingerprint | the binding of imported instances (§3.3) |

- `init` is the first entry of the first record, and occurs nowhere else.
- A `call` entry stands in the `calls` of an `init`, `migrate`, `mut`, or
  `call` entry. A `mut` entry whose call came from a `call` or `host` entry of
  the record stands in that entry's `calls`. Every other entry stands in
  `trace`.
- A **mutation path** leads from the instance root to the mutation along the
  way that the call came: through the row that it runs on (§5.3), or, when it
  runs on several rows, through the set or the view that reached them. The
  running root's mutation paths include `/add`; `/lines/2/void`, on the second
  line; `/lines/void`, on every line; and `/vat/settled`, the mutation that the
  root provides at its `vat` mount point.
- A call has one entry, also when it runs on several rows, and none when it runs
  on zero rows.
- A **callee** is a mutation's declaration path (LANGUAGE 8), along the way
  that the call came for another instance's mutation: through its mount point,
  as in `/vat/quote` and `/parent/settled`, or through the view that reached
  the mount points; or a host mutation's qualified name, as in `ticket` and
  `mail.send`.
- An **artifact fingerprint** is the 64-byte SHA-512 digest of the artifact's
  ZIP bytes.

The version after a record is the version of its last `init` or `migrate`
entry, or else the version before the record.

### 5.3 Changes

A **path** reaches stored state from the root. The root is `/`, and each
further segment is a member's name or, after the name of a set, a row number:
`/lines` is the set of lines, `/lines/2` the line numbered 2, and
`/projects/1/tasks` the tasks of the project numbered 1. A path that leaves out
a row number passes through every row of that set: `/projects/tasks` reaches the
tasks of every project. A row's **number** is its rank among the rows that the
history inserts into its set: the first is 1, the next 2, and a number is never
reused. A set keeps its numbering through a rename, and when the object that
holds it is removed and constructed again.

A record's `changes` is an array of single-entry maps. Each change applies to
the state that the changes before it leave:

| Change | Effect |
| --- | --- |
| `{p: m}`, with `p` reaching the root, objects, or rows | Each member that `m` names takes its value, in each of them. |
| `{p: s}`, with `p` reaching a set | The rows change as `s` states, below. |
| `{"rename": [a, b]}` | The member at the declaration path `a` becomes `b`, in every owner, keeping its values and rows; `b` differs from `a` in its last name. |
| `{"drop": a}` | The member at the declaration path `a` leaves every owner, with what it owns. |

In `{p: m}`, `m` is a **merge**: a map from member names to values. A value is
encoded as in §4.1, and holds each set as `[]`: a set's rows come from inserts
at the set's own path. A value replaces the member's value; `null` makes an
optional member missing, and removes an object that it held with what the
object owns. A member can also take a **computed value**, `{"=": e}`, with
optional `with`. A row of a set of scalars, such as `Text*`, takes its new value
or a computed value in place of a merge.

In `{p: s}`, `s` is a **set change**, with up to four parts, which apply in
this order:

1. `remove` lists row numbers and filters. The rows that they select leave the
   set, with what they own.
2. `update` lists filters, each with `set`, a merge. Each row that a filter
   selects takes the values of its `set`, all of them computed from the state
   before that update.
3. `assign` lists row numbers, each with a merge that the row of that number
   takes.
4. `insert` lists values, which join the set, in order, as new rows.

A **filter** is a map with an optional condition, `where`, and optional `with`.
It selects the rows for which the condition holds, or every row when it has
none. At a path that passes through every row of a set, a set change has no
row numbers and no `insert`.

A condition and the `e` of a computed value are LANGUAGE expressions (A.4),
evaluated in the row or object to which they apply: `.` is that row or object,
`^` its owner, and `/` the root. They read stored state through the
declarations of §2.2, literals, and, as `@name`, the values of `with`: a
computed value's own, and in an update also its filter's. A value's encoding
gives its type: a text is a `Text`, `true` and `false` are `Bool`s, `null` is
missing, an array is a set, and a map a record. Integers are exact: the
operators of §2.2 apply to them without the bounds of `I64` and `U64`. A
condition gives a `Bool`.

On the running lines, voiding every line is
`[{"/": {"net": 0}}, {"/lines": {"update": [{"set": {"amount": 0}}]}}]`;
doubling each amount is
`{"/lines": {"update": [{"set": {"amount": {"=": ".amount * 2"}}}]}}`; removing
the lines of zero amount is
`{"/lines": {"remove": [{"where": ".amount == 0"}]}}`; and setting both amounts
is `{"/lines": {"assign": [[1, {"amount": 10}], [2, {"amount": 20}]]}}`.

### 5.4 Building

An implementation records each commit, in each participant, from the run that
committed:

1. The trace holds each call that ran in the instance, in the order in which
   they ran, placed as §5.2 states. Function calls, reads, and checks add no
   entry.
2. The changes, applied in order to the state before the commit (§5.5), give
   the state after it. The implementation chooses their form: rows by number or
   by filter, values or computed values.
3. An argument or a drawn result appears in the changes only as a value that
   the commit stores.

The running root took part in seven commits: its construction, the two `add`
calls of one transaction, `void()` on the second line, `settle()`, `issue()`,
`remind()`, and the move to `1.2.0`. Its history is:

```cbor-diag
{
  "meta": {"module": "acme.invoice", "uuid": 37(h'00112233445546778899aabbccddeeff')},
  "records": [
    {
      "commit": 37(h'00000000000040008000000000000001'),
      "instant": 100,
      "trace": [{"init": "1.0.0"}],
      "changes": [
        {"/": {"lines": [], "net": 0, "tax": 0, "total": 0, "settlements": 0, "number": null,
               "vat": {"module": "acme.tax", "instance": 37(h'66778899aabb4ccd8eef001122334455')}}}
      ]
    },
    {
      "instant": 200,
      "trace": [{"mut": "/add"}, {"mut": "/add"}],
      "changes": [
        {"/": {"net": 80}},
        {"/lines": {"insert": [{"label": "Paper", "amount": 40}, {"label": "Paper", "amount": 40}]}}
      ]
    },
    {
      "instant": 300,
      "trace": [{"mut": "/lines/2/void"}],
      "changes": [{"/": {"net": 40}}, {"/lines/2": {"amount": 0}}]
    },
    {
      "commit": 37(h'00000000000040008000000000000004'),
      "instant": 400,
      "trace": [
        {"mut": "/settle", "calls": [{"call": "/vat/quote", "calls": [{"mut": "/vat/settled"}]}]}
      ],
      "changes": [{"/": {"settlements": 1, "tax": 8, "total": 48}}]
    },
    {
      "instant": 500,
      "trace": [{"mut": "/issue", "calls": [{"call": "ticket"}, {"call": "mail.send"}]}],
      "changes": [{"/": {"number": "INV-7"}}]
    },
    {
      "instant": 600,
      "trace": [{"mut": "/remind", "calls": [{"call": "mail.send"}]}]
    },
    {
      "instant": 700,
      "trace": [{"migrate": "1.1.0"}, {"migrate": "1.2.0"}],
      "changes": [{"rename": ["/lines/label", "/lines/title"]}, {"/": {"currency": "EUR"}}]
    }
  ]
}
```

The child took part in the first and fourth commits, so those records carry
`commit`. The two equal lines are rows 1 and 2, and `void()` ran on row 2. In
the fourth record, the call back from the child stands in the call that left
the root, and the tax that the root read from the child is a stored value.
`remind()` changed no state, so its record holds a trace alone, with the effect
call to `mail.send`. The move holds one `migrate` entry for each step. Moving
back to `1.0.0` later would record:

```cbor-diag
{
  "instant": 800,
  "trace": [{"migrate": "1.1.0"}, {"migrate": "1.0.0"}],
  "changes": [{"drop": "/currency"}, {"rename": ["/lines/title", "/lines/label"]}]
}
```

The child's history is:

```cbor-diag
{
  "meta": {"module": "acme.tax", "uuid": 37(h'66778899aabb4ccd8eef001122334455')},
  "records": [
    {
      "commit": 37(h'00000000000040008000000000000001'),
      "instant": 100,
      "trace": [{"init": "1.0.0"}],
      "changes": [{"/": {"rate": 20, "amount": 0, "tax": 0}}]
    },
    {
      "commit": 37(h'00000000000040008000000000000004'),
      "instant": 400,
      "trace": [{"mut": "/quote", "calls": [{"call": "/parent/settled"}]}],
      "changes": [{"/": {"amount": 40, "tax": 8}}]
    }
  ]
}
```

### 5.5 Reading and replay

**Replay** reconstructs an instance from its history: starting from an empty
root, it applies the changes of each record in order. The state after a record
is the committed state after its commit, at the version that its trace gives
(§5.2). Replay runs no module code and performs no effect. With both files
present, the final state equals the data, compared as decoded values, and the
final version is the data's.

Readers MUST reject a history that breaks Annex A or §§5.1–5.3, including:

- an entry out of its place, or a version that the package does not carry;
- a path, row number, or input that does not resolve where its change applies;
- a `rename` from a missing member or onto an existing one, and a `drop` of a
  missing member;
- a value that holds a nonempty set;
- an expression that calls anything other than a declaration of §2.2, or whose
  evaluation fails;
- a record after which the state is off its version's stored structure.

## 6. Conformance

Host libraries (LANGUAGE 7.4) supply declarations beyond the builtins, and the
implementation that runs an artifact supplies them. An artifact whose
definitions use a host declaration works on an implementation that supplies
that declaration with the same meaning, call class, and representation (§4.1).
An implementation MUST reject an artifact that uses a host declaration that it
lacks, and name the declaration.

Every implementation produces these outcomes for the running application:

| Case | Outcome |
| --- | --- |
| Replay the root's first six records | The data of §4.2. |
| Replay all seven of its records | Version `1.2.0`; the lines have `title` in place of `label`, and `currency` is `"EUR"`. |
| Replay the root's history with the child absent | The same states: replay reads the root's history alone. |
| Run the two `add` calls of the second record as concurrent transactions | `net` is `80` in either commit order. |
| Read the views that the two `add` calls returned, after `void()` on the second line | The first reads `{ label: "Paper", amount: 40 }`, the second `{ label: "Paper", amount: 0 }`. |
| Call `remind()` | One record, with the trace of the sixth record of §5.4 and no changes; unchanged state; one effect. Replaying or importing the history performs zero effects. |

Every reader rejects the running artifact after any one of these changes:

- an entry path with a `..` segment;
- a floating-point `amount`, or a duplicate map key, in a data file;
- a data file without `settlements`, or with a member that its version lacks;
- bytes after a file's map;
- a data `version` other than the one that the history reaches;
- `"changes": []` in the second record, or a record without `trace`;
- an entry with both `mut` and `call`;
- an `init` entry in the second record, or a `migrate` entry inside a `mut`
  entry;
- the change `{"/lines/3": {"amount": 0}}` in the third record;
- `{"lines": [{"label": "Paper", "amount": 40}]}` in the merge of the first
  record;
- the change `{"rename": ["/lines/label", "/lines/amount"]}` in the seventh
  record;
- the value `{"=": ".vat.quoted"}` for `tax`, or `{"=": "@fee"}` without
  `with`, in the fourth record.

## Annex A. History schema

This schema of `history.cbor` uses CDDL
([RFC 8610](https://www.rfc-editor.org/rfc/rfc8610.html)). Chapter 5 gives the
meaning of each part, and `value` is a value encoded as §4.1 states.

```cddl
history = {
  meta: { module: tstr, uuid: uuid },
  records: [+ record],
}

record = {
  ? commit: uuid,
  instant: int,
  trace: [+ entry],
  ? changes: [+ change],
}

entry = {
  ( init: version
  // migrate: version
  // mut: tstr
  // call: tstr
  // host: tstr
  // extract: fingerprint
  // import: fingerprint ),
  ? calls: [+ entry],
}

change = { path => merge / set-change }
       / { rename: [path, path] }
       / { drop: path }

merge = { + tstr => value / computed }
computed = { "=": expression, ? with: inputs }

set-change = {
  ? remove: [+ number / filter],
  ? update: [+ { filter-fields, set: row-change }],
  ? assign: [+ [number, row-change]],
  ? insert: [+ value],
}
row-change = merge / computed / value
filter = { filter-fields }
filter-fields = ( ? where: expression, ? with: inputs )
inputs = { + tstr => value }

path = tstr .regexp "/.*"
number = uint .ge 1
version = tstr
expression = tstr
uuid = #6.37(bstr .size 16)
fingerprint = bstr .size 64
value = any
```
