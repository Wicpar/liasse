# History, commits, integrity, and erasure

Status: standard.

## 1. Commits: the canonical form

Every admitted mutation call produces one non-empty commit. Canonically:

```json
{
  "$id": "9f2c…",
  "$parents": ["a11a…"],
  "$time": "2026-07-06T18:40:11.302Z-0007",
  "$actor": "/users/u1",
  "$via": "/companies/acme/auth/sessions/s-77",
  "$reason": "left the team",
  "$gen": { "comments.id": "c9" },
  "$basis": { "companies/acme": { "members/u1": "sha512:44d1…" } },
  "$ops": {
    "companies/acme": {
      "projects/website": { "status": "archived", "lead": { "$=": { "$none": true } } },
      "comments": {
        "c1": { "author": { "$=": { "$none": true } }, "body": "[deleted]", "deleted": true },
        "c9": { "$=": { "body": "Member offboarded", "deleted": false } }
      },
      "members/u2": { "$-": true }
    }
  },
  "$state": "sha512:e07a…"
}
```

`$ops` is a **delta tree** — the shape of the mutation grammar itself:

```text
shared path prefixes appear once
a leaf value sets a value; { "$=": value } replaces the whole subtree
{ "$none": true } encodes the expression-level none sentinel in canonical values
{ "$-": true } deletes a field or row; canonical ops do not use JSON null
statement boundaries do not survive lowering: a mutation sequence and its
  cascades are one op tree, one atomic outcome — any failure rejects all
```

Rules:

```text
$actor is mandatory; module writes carry the instance path, $as the human
$via records the carrying session (audit: which device)
$gen records generative results (uuid(), now()) once, so replay is deterministic
$basis holds hashes of data read but not written; everything a mutation
  read is its lock, re-verified before admission (MUTATIONS 12)
$prior is VIRTUAL: the predecessor's value at the path, found via the
  temporal index — defined, never stored, duplicated never; history is
  bidirectional (forward applies values, backward applies priors) at
  zero storage cost
```

## 2. Two integrities

```text
$id      history integrity — a structural hash over the full canonical
         commit envelope: parents, time, actor/via/as/reason, generated
         values, basis, ops, and versioned encoding. Each value and path
         segment contributes through its hash. Computed, not stored twice:
         while bytes are present the hash is derivable; it materializes in
         storage only where erasure replaces bytes with a stub (§7).
         Invariant under stubbing by construction.

$state   state integrity — the Merkle root of the whole tree at the
         frontier after this commit. 64 bytes per commit. Proves what the
         data was; historical reconstructions verify against it even when
         leaves are stubbed (a stub contributes exactly its retained hash).
         Snapshot bundles and fast-forwards verify against $state instead
         of being trusted.
```

## 3. Versioning of encodings

Every serialized artifact — bundle, history segment, extract file — begins
with its `$liasse` wire version. `$id` and `$state` are pinned to the hash
definition of the version the commit was created under, recorded in the
carrying artifact's header; a DAG may mix eras and still verify.
**Canonical is not stored**: engines intern paths, pack HLCs, store raw
hashes, compress segments — and may re-encode freely across versions.
`$bytes_history` counts canonical value bytes written (deterministic,
billable); physical footprint is `store.usage`'s business.

Normal-path overhead is therefore structural only: a `"480"` in history
costs five bytes plus coordinates —

```text
$bytes_history ≈ Σ canonical value bytes written + O(coordinates per op)
```

## 4. The DAG and the fold

Commits form a hash DAG (`$parents` = seen heads). **State is a deterministic
fold of the admitted commit set**: canonical linearization — topological by
causality, ties by `($time, $id)`, HLC-issued `$time` — each commit applying
at its place.

A proposed commit is admitted only if, at that canonical place, its `$basis`
still matches, every keyed target exists, every check passes, every meter and
storage invariant holds, and all static delete-completeness requirements are
satisfied. A failed proposal is rejected deterministically and does not enter
the DAG, history, `$state`, or bundle exports. Gateways may keep rejection
reports for clients, but those reports are transport/audit events, not model
history.

Two stores holding the same admitted commits compute byte-identical state;
sync is set union over admitted commits. Rejected proposals are ignored by
all replicas for the same reason and need no empty tombstone commit. Every
selector traversal adds traversed ancestors' existence to the basis, so
delete/write races resolve deterministically.

Model and package changes use the same gate. Concurrent migrations produce
one admitted migration and one rejected proposal.

## 5. History as data

```text
path.$history      rows { $time, $actor, $commit, $value, $prior } in
                   canonical order ($prior derived)
```

`$history` is a pure function of the admitted commit set — model views,
surface-gated, and composable with the view combinators. A client may also
ask the engine to render a granted view at a historical frontier as a request
parameter; that is client protocol, not an authoring expression.

History surfaces are grants like any other: an `$on_delete` expression hides
data from the live base, not from history readers whose surfaces reach
`$history` — until erasure (§7) removes the bytes vertically.

`path.$bytes` includes retained history (`$bytes_live` / `$bytes_history`
break it down) — history is a billed resource.

## 6. `$history` tiering

A middleware of its own, sibling of `$blob_storage`, inherited nearest-wins:

```hjson
"$history": {
  "$hot": "P3M",
  "$segment": "P1M",
  "$tiers": [
    { "$after": "P3M", "$in": "/stores['s3']" },
    { "$after": "P2Y", "$in": { "$copies": 2, "$of": "/stores[:s | s.class == 'cold']" } }
  ],
  "$trim": { "$after": "P10Y" }
}
```

Segments are canonical, content-addressed blobs — placement, sha512 integrity,
migration, and real-location billing come from `STORAGE.md`. `$trim` vacuums
content beyond retention to hashes; never applicable inside a declared legal
retention window. Reads through archived ranges are transparent, slower, and
permission-checked.

## 7. Erasure: extraction as a revertible delete

Deletion policy lives **on the ref**, governs every delete of the target, and
has no default:

```text
"$on_delete": "restrict"     an explicit business rule: deletion is
                             rejected while referenced
"$on_delete": "cascade"      the row goes with the target
"$on_delete": "= expr"       a patch of the containing row, evaluated
                             over self, shape-checked at load
                             (clearing a ref is { field: none })
```

**Static completeness**: a ref with no declared policy is legal only while
nothing can delete its target. If any mutation group, exposed surface,
module-provided mutation, or migration can delete the target, the declaring
package fails to load, naming the undecided refs. A migration containing an
incomplete delete cannot exist. Crossing a module boundary does not weaken
this: a module that exposes a mutation must decide the delete policy for every
ref that mutation can affect before the surface is exposeable.

**Extraction** is deleting a row with its cascades — one ordinary, atomic
commit — plus the vertical scrub:

```text
scrub      every occurrence of the affected values and key segments — live,
           historical $value and (virtual) $prior, hot and archived — has
           its bytes replaced by the stub ~sha512:… (a literal ~ escapes as
           '~). $id and $state verify unchanged: structural hashing makes
           stubbing hash-transparent; no segment surgery, no supersession
keys       key segments are always stubbed, never expr-replaced; refs storing
           the key rewrite to the same stub in lockstep — hash-consistent
           substitution keeps identity, reachability, $unique, and joins
           mechanically sound
extract    the removed bytes, keyed by stub, form the extract file; the
           erasure commit records only internal data:
           { "$extract": { "$file": "sha512:…", "$stubs": { "path@commit": "hash" } } }
attest     signatures are commits — { "$attest": { "$of": "commit",
           "$signer": "…", "$alg": "…", "$sig": "…" } } — signing
           the file hash, structurally outside the file, appendable later,
           verified against embedder trust anchors
reinsert   verify file hash and attestations first; then restore requested
           values only where the current occurrence is still its stub; any
           mismatch rejects the proposal. The accepted reversion is a commit
           referencing the extract commit
```

The base never learns erasure happened: after the commit it is
indistinguishable from a base that always held the `$on_delete` results —
fully typed, no markers. Who may erase is who holds a surface exposing the
delete; ownership is the permission system.

## 8. Bundles

```json
{
  "$liasse": 1,
  "$bundle": {
    "$path": "/companies/acme",
    "$mode": "snapshot|log|full",
    "$heads": ["…"],
    "$base": [],
    "$time": "…",
    "$external": { "…": { "$schema": "…" } }
  },
  "$model": {},
  "$data": {},
  "$history": []
}
```

A snapshot restores and **verifies against `$state`**; a log fast-forwards any
store whose causal past is contained in `$base` (diverged import = union +
refold); full is both. Any subtree exports; bundles are closed (module
instances, install records, `$extracts` stubs included) and declare external
requirements first. `export(import(x)) == x`.

## 9. Library surface

```text
load / migrate                  commit(actor, reason, ops|call)
get / query / watch(since)      export / import
modules.{list,install,bind,update,enable,disable,uninstall}
erase(row) -> extract file      reinsert(file)
store.usage / store.placement   physical lenses, never model views
```
