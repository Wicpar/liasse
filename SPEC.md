# Liasse Specification

The interoperability contract for `liasse 0.0.1`: the `.liasse.zip` format and
system guarantees. [LANGUAGE.md](LANGUAGE.md) defines source syntax and meaning;
[BUILTINS.md](BUILTINS.md) defines the core library contracts. Backends choose
how they store data and execute operations.

**MUST**, **MUST NOT**, and **MAY** have their BCP 14 meanings. Examples are
informative.

## 1. System guarantees

An **instance** is a module with its own stored state and history. An
**application** is a root instance and its mounted descendants. The **host**
runs the application and authorizes access to its views and mutations.

### 1.1 Transactions and reads

A transaction runs over one snapshot, with its own earlier writes visible to
later statements. It spans every instance reached by its calls. Its complete
result MUST satisfy the language's types, constraints, and module interfaces
before it becomes visible as one atomic **commit**. A rejected transaction
leaves state unchanged. Initialization, import, and updates obey this rule too.

Commits have one order. Each read observes one consistent snapshot; each read
or transaction started after an acknowledged commit observes that commit.
Concurrent transactions and their results MUST be equivalent to a serial
execution consistent with that observation order. An acknowledged commit is
durable. Backends MAY serialize work, retry transactions, or pause work during
updates.

A transaction that changes no state and calls no effects is **unchanged**.
Otherwise a successful transaction is **committed**. Returned snapshots and
views have the meaning given in LANGUAGE 5.4. A failed evaluation or host call
rejects the operation with a diagnostic identifying the cause.

Each read or transaction has one instant, fixed at its start. A retried
transaction uses its new run's snapshot and instant; only the successful run's
result and effects survive. A commit records that run's instant. Instants
follow the host's wall clock, which MAY move backwards; commit order determines
history order.

A live view is read again when a commit or the passage of time changes its
result. Its successive snapshots include the commits observed by earlier reads.
The host MUST check authorization against the snapshot used by each read or
call; an interface grants access only to the members it exposes.

### 1.2 Host calls

Call classes and permitted contexts are defined in LANGUAGE 4.4. A drawn call
runs anew on each evaluation. An effect is performed only after its transaction
is durable, including when that transaction changes no stored value.

The host MUST retain pending effects across restarts. It attempts each effect
until it succeeds or reaches its configured attempt limit, then reports a
failure. Every attempt carries the same effect identifier, distinct from other
effects' identifiers, so the receiver can recognize retries. Effects MAY run in
any order. Pending effects belong to the running system and are excluded from
exports.

### 1.3 Identities and paths

Each instance has a UUID, preserved by export and import. Each row has an opaque
text identity, stable for its lifetime and unique within its owner's stored
set while it exists. Its identity contains 1–255 ASCII letters, digits, or
characters from `-._~`, excluding `.` and `..` alone. A backend MAY reuse a
removed row's identity.

A **state path** starts with `/` and alternates declaration names with row
identities after stored sets, as `/companies/k7Qx2m/offices/p2Rt8n`. Paths in
host operations start at the application's root; paths in an instance's history
start at that instance. A scalar row that is itself a set has another row
identity as its next segment. Removing a row's owner
removes its nested state; renaming a declaration preserves surviving identities.

### 1.4 Host libraries

The host injects `.sys.liasse` libraries. They supply the core contracts in
BUILTINS and MAY add types, functions, and mutations. Their declarations and
implementations stay outside the archive. A destination host must supply the
contracts a module uses before running it; adapting source or providing shims
is acceptable. A signature alone does not establish equivalent behavior.

### 1.5 Diagnostics

Every failure gives a code and an explanation. The code identifies its cause:

| Code | Cause |
| --- | --- |
| `malformed` | Invalid input or archive data |
| `build` | Invalid definitions or incompatible library or module contracts |
| `constraint`, `uniqueness`, `assertion` | A failed check of that kind |
| `evaluation` | An undefined computation or exceeded value or evaluation limit |
| `host` | A host operation failed or was refused |
| `contention` | The configured retry or update-search limit was reached |
| `denied` | The caller lacks access |

A literal outside its type's limits fails at build time, a received value is
malformed, and a computed value fails evaluation. Client diagnostics MUST expose
only information the caller is authorized to receive.

## 2. The `.liasse.zip` archive

A **package** carries one module's sources and migration chain. An export adds
instances and their data. Both use a ZIP archive with stored or deflated entries;
readers MUST accept ZIP64. Entries are unencrypted. Source files are UTF-8.

```text
acme.tasks.liasse.zip
  tasks.liasse                         current module definition
  limits.lib.liasse                    its source libraries
  migrations/
    1.0.0/tasks.liasse                 earlier versions and their libraries
    1.0.0/limits.lib.liasse
    1.1.0/tasks.liasse
    1.1.0/limits.lib.liasse
    1.0.0-1.1.0.migration.liasse
    1.1.0-1.2.0.migration.liasse
  instances/
    <uuid>/
      data.cbor                      current state
      history.cbor                   history, when requested
      blobs/<digest>                 this instance's blob bytes
      submodules/
        editor/acme.text.liasse.zip   child at /editor
```

The root holds exactly one module definition; its `meta` supplies the name,
current version, and language version. Library files share its directory.
Versions under `migrations/` have the same arrangement. The versions form a
chain in numeric version order; consecutive versions with different stored
structures have a migration between them. Metadata and filenames MUST agree
where a filename names a module or version.

A module name consists of dot-separated components, each a lowercase ASCII
letter followed by lowercase letters, digits, or `_`. A version has three
decimal integers separated by dots, with no leading zeros except `0` itself;
compare major, minor, then patch numerically. The language is `liasse <version>`.
The earliest carried version is the chain's **floor**.

Each instance has one folder, named by its UUID in lowercase `8-4-4-4-12`
text form. Its nested packages hold its children, recursively. Module sources
and migrations are shared by the instance folders in their package. A package
with no instance folders supplies definitions for initialization.

Split or combine packages of the same module by copying their instance folders
alongside the matching sources and migrations. Copying or deleting a whole
instance folder carries or removes its data, history, blobs, and child ZIPs
together. Each nested ZIP is a complete module package that can be extracted
and imported on its own.

The file tree determines current mounts. A child ZIP is under
`submodules/<mount-path>/`, with the path relative to its owning instance:
`submodules/users/ana/mail/acme.mail.liasse.zip` mounts at `/users/ana/mail`.
At most one instance with `data.cbor` may occupy each mount path. Moving, adding,
or deleting a child ZIP changes that mount; an empty optional mount has no live
child ZIP. Required mounts and both interfaces must still be satisfied.

### 2.1 Export and import

An export selects one or more instances of a module and reads them at one
snapshot. It carries those instances and their mounted descendants. Sharing a
module name alone never includes another instance.

The caller chooses **current state** or **current state with history**. Both
include current state, its definitions, migrations, and referenced blob content.
With history, the export also includes their retained histories and descendant
histories, with the definitions and blobs those histories need. Historical
instances that have been unmounted carry definitions and `history.cbor` under
their last mount path, with no `data.cbor`. A descendant's history is included
for the lifetime of that instance. Historical references use UUIDs, so moving
or removing a child package leaves the other instances' history readable.

Each instance MUST carry in its own `blobs/<digest>` the bytes referenced by
its exported data and history, including package blobs named by history. Names
are lowercase hexadecimal digests, with one entry per content in that instance's
folder. Shared content is copied into every instance folder that references it.

Export retains access to the selected snapshot's blob content until it has
copied it. Export succeeds only when every required blob has been copied and
verified against its digest and any recorded size; unavailable or corrupt
content fails the export. Selecting current state alone includes every blob
referenced by current state, including inside nested objects and sets.

Import loads the recorded current state, including identities and row metadata,
the included history, and the blob bytes. It verifies and stores required content
durably in the destination before exposing the imported state. It MUST preserve
typed values, element order, duplicates, and the file tree's mounts.
Initializers and mutation calls are not rerun. A current-state import starts
retained history at the imported snapshot; earlier history is unavailable.
It preserves the rows' original timestamps.

The destination chooses where to mount the exported roots, satisfying their
interfaces and parent contracts. Import MUST reject an instance identity already
present in the destination's state or history, conflicting definitions of the
same module version or migration, or a mount point already occupied.

Readers MUST reject malformed CBOR or source, unsafe or duplicate ZIP paths,
missing required definitions or content, invalid types or metadata, repeated
live row identities within one owner, cycles or multiple ownership of instances,
and blob content that disagrees with its digest or size. An unsafe path is
absolute or contains a backslash, an empty segment, `.` or `..`; a trailing `/`
is allowed for a directory entry. Unknown entries MAY be ignored. Validate the
whole import before exposing its state.

## 3. CBOR data

Each `.cbor` file contains one CBOR data item
([RFC 8949](https://www.rfc-editor.org/rfc/rfc8949.html)). Maps name the parts of
the structure; tables carry repeated rows with their column names written once.
The module's declared types give values their interpretation. Internal storage
is the backend's choice.

### 3.1 Values

| Value | CBOR representation |
| --- | --- |
| `Bool` | `false` or `true` |
| `Int` | Integer; beyond the range of major types 0 and 1, tag 2 or 3 bignum |
| `Decimal` | Tag 4 over `[exponent, mantissa]`, in minimal-scale form |
| `Text`, `Bytes` | Text string, byte string, respectively |
| `Uuid` | Tag 37 over 16 bytes in RFC 9562 order |
| `Date` | Tag 1004 over an RFC 3339 full-date text |
| `Timestamp`, `Duration` | Signed integer microseconds; timestamps are since the Unix epoch |
| `Enum` | Its text label |
| Record or object | Map with text keys, one for each present stored member |
| Set | Array of elements, preserving order and duplicates |
| Missing optional | Omitted record member, or `null` in a position that cannot be omitted |
| Present optional | Its inner encoding; if the inner type is optional, a one-element array containing that encoding |
| `Blob` | Map with `digest` (64 bytes) and `size` (nonnegative integer) |
| `Module` | Map with `module` (module name) and `instance` (UUID encoded as above) |

An empty set is present and is encoded as `[]`. Thus a missing `Text*?` member
is omitted, a present empty one is `[]`, and `Text**` distinguishes `[]` from
`[[]]`. A missing inner optional inside a present optional is `[null]`.

Readers accept equivalent integer widths, lengths, map-key orders, and decimal
scales. A writer uses the forms above. Duplicate map keys, invalid UTF-8,
floating-point values, unsupported tags or simple values, and items outside
the expected type's values are malformed. Other host types need a documented
CBOR representation and a compatible destination library or explicit conversion.

### 3.2 Instance state

An instance's `data.cbor` is a map with `version`, its active version as text,
and `root`, a map of its stored members. Objects use named members recursively.
Views, functions, `Module`, and `Parent` members are omitted; child ZIPs supply
mounts (§2).

A stored set is a map with `columns` and `rows`. The `columns` array holds text
names for the cells of each row; `rows` is an array of rows in stored order.
Both keys are required, including for an empty set, whose `rows` is `[]`.

| Element type | Columns |
| --- | --- |
| Object | `["$id", "$created_at", "$updated_at", member_1, ..., member_n]` |
| Any other type | `["$id", "$created_at", "$updated_at", "$value"]` |

Metadata names start with `$` to distinguish them from declared members. Row
identity and timestamps have the meanings in §1.3 and BUILTINS §5. Object
columns include every encoded member, even when its value is missing. Writers
put metadata first, then member names in ASCII order. Readers match columns by
name. Missing, duplicate, or invalid columns and incorrect row widths are
malformed. The same table rules apply to history batches.

Cells follow the declared type: objects are maps, nested stored sets are tables,
and other values use §3.1. A missing optional member is omitted from an object
map, or written as `null` in a table cell. A present optional uses its inner
encoding, including the extra wrapper for nested optionals.

For a module whose only stored member is `tasks`, a set of objects with
`done: Bool` and `title: Text`, this instance contains two equal tasks with
different identities (CBOR diagnostic notation):

```cbor-diag
{
  "version": "1.0.0",
  "root": {
    "tasks": {
      "columns": ["$id", "$created_at", "$updated_at", "done", "title"],
      "rows": [
        ["a", 100, 100, false, "Review"],
        ["b", 100, 100, false, "Review"]
      ]
    }
  }
}
```

The two tasks remain separate rows in that order, with their original timestamps.

### 3.3 Instance history

An instance's `history.cbor` is a map with `transactions`, an array in commit
order. Each successful transaction is a map with these fields:

| Field | Value |
| --- | --- |
| `instant` | Transaction instant, as integer Unix microseconds |
| `request` | Origin of the transaction: `"call"`, `"native"`, `"create"`, `"import"`, or `"update"` |
| `calls` | Top-level mutation calls into this instance, in execution order |
| `changes` | Bulk operations on this instance's state or composition |

Each call is a map with `path`, the receiver's path within this instance, and
`mutation`, the mutation's name. Log calls initiated by a client or host and
calls entering from another instance, including through `Parent`. Calls between
mutations inside this instance remain internal. Arguments and returned results
stay out of history; values deliberately stored appear in the changes.

A successful transaction records an entry when it changes the instance or enters
through a logged call. Thus a mutation that only calls a child still appears in
the parent's history, with empty changes. Rejected or cancelled transactions
record no entries; retries contribute only the successful run. The affected
instances' state and history become durable atomically before acknowledgment.

The `calls` and `changes` fields are ordered arrays, empty when there are none.
Each change is a map with `op` and the fields below. Bulk changes name their
target and columns once for all their rows. Every path starts at the owning
instance. File, transaction, call, and operation maps require the fields shown;
missing or extra fields are malformed.

| `op` | Other fields | Meaning |
| --- | --- | --- |
| `init` | `version`, `root` | Record the instance's starting data, as in §3.2 |
| `insert` | `path`, `columns`, `rows` | Append full rows to a stored set, using §3.2's columns |
| `remove` | `path`, `ids` | Remove the listed row identities from a stored set |
| `write` | `path`, `columns`, `rows` for a set; `path`, `values` for an object | Write the named columns or members |
| `mount` | `columns`, `rows` | Columns `mount_path`, `module_name`, `child_uuid` identify the children and their mounts |
| `unmount` | `paths` | Clear the listed mount points |
| `give` | `digests` | Record packages supplied to this instance, in order |
| `move` | `version` | Carry structure along its migration route before the following value changes |

For a stored-set write, `columns` includes `$id` to select each existing row and
the columns being written; `$created_at` is excluded. Writers put `$id` first.
Identity and creation time are preserved. For a root or object write, `values`
is a map of members to write; `null` clears an optional member. Unlisted members
or columns stay unchanged. Changed row metadata, including `$updated_at`, is
written explicitly through its stored set. The `ids`, `paths`, and `digests`
fields are arrays.

Batch compatible operations together, preserving dependencies and insertion
order; separate batches when necessary to do so. Changes apply in array order
under the active version at that point. A module-wide package update records
`give` for each affected instance. UUIDs and digests use §3.1; package digests
refer to bytes in that instance's `blobs/`.

For the tasks in §3.2, a root mutation named `complete_all` completes both
tasks. Its transaction appears in history as follows (earlier transactions
omitted):

```cbor-diag
{
  "transactions": [
    {
      "instant": 200,
      "request": "call",
      "calls": [{"path": "/", "mutation": "complete_all"}],
      "changes": [
        {
          "op": "write",
          "path": "/tasks",
          "columns": ["$id", "$updated_at", "done"],
          "rows": [["a", 200, true], ["b", 200, true]]
        }
      ]
    }
  ]
}
```

Mounting or unmounting a child changes the parent's history. Each instance's
first record contains `init`, used once. Its history stays unchanged when
exported as a root. A current-state import starts each instance's history with
request `"import"`, empty calls, `init` holding its imported data and original
row metadata, and a mount batch for its children.

Applying an instance's batches reconstructs its states, identities, order, row
metadata, and mounts. On export, a live instance's history MUST end at its
current state; an unmounted instance's history preserves its last state.
Import reads current data directly. If the file tree changes recorded mounts,
import appends the corresponding mount and unmount batches with request
`"import"`, applying the usual row timestamp rules. Other recorded data must
agree with history. An omitted child's past state is unavailable; its UUID
remains in the parent's history. Backends MAY store history however they choose
while preserving these results. Pending effects are never replayed.
