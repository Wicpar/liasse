# Blobs and storage

Status: standard. The blob primitive, stores, placement requirements,
reconciliation, the blob meta API, integrity, serving, and billing.

## 1. The blob primitive

A `blob` is opaque large binary data, never processed in the database. The
value in a row is a **descriptor**:

```json
{ "$sha512": "ab31…", "$bytes": "184320",
  "$media": "application/pdf", "$name": "facture-113.pdf" }
```

```text
$sha512 is the identity: store object key, dedup unit, verification target
expressions read descriptor fields, never content — fold purity is
  structural; equality is hash equality
you never edit a blob; you reference a new hash
$prior of a replaced blob field is the old descriptor; bytes never ride
  commits; bundles carry descriptors, bytes as an optional sidecar
  manifest keyed by $sha512
$name is client-supplied, normalized, untrusted
```

As a parameter type, blob declares acceptance constraints, enforced while
bytes stream in — before anything is retained:

```hjson
"pdf_t": { "$type": "blob", "$max_bytes": "10485760",
           "$media": ["application/pdf", "image/jpeg", "image/png"] }
```

## 2. Connectors and stores

```text
connector   embedder-registered code (s3, gcs, fs) — the hard capability;
            declares its interface: presign_get/put, ranges, checksum mode
store       connector + parameters = an ordinary row; params may hold
            vault references ("$vault:acme-archive"), resolved at the
            connector boundary — plaintext never sits in data or bundles
```

```hjson
"stores": { "$key": "id", "id": "text", "connector": "text",
            "params": "json", "enabled": "bool" }
```

Declare the bank shape once as a type and use it at the root and inside
companies — no donor structure needed:

```hjson
"$types": { "store_bank": { "$key": "id", "id": "text",
                            "connector": "text", "params": "json",
                            "enabled": "bool" } },
"stores": "store_bank",
"companies": { "…": "…", "storage": { "stores": "store_bank" } }
```

Companies bring their own buckets by inserting rows, permission-gated by
an ordinary surface. An unknown connector name is legal data; it makes the
store unavailable — an operational diagnostic, never a rejected commit.

## 3. `$blob_storage`: the placement requirement

Declared on any row shape — middleware, not field config: inherited by
every blob beneath, nearest declaration wins, field-level override only
for exceptions. One mandatory key and one optional:

```text
$in      the placement requirement (grammar below)
$serve   read preference order; defaults to $in order
```

The requirement grammar — one recursive definition, fully composable:

```text
placement := view                          in EVERY store the view yields
           | { "$all": [placement, …] }    and
           | { "$any": [placement, …] }    or — ordered; the FIRST
                                           alternative is where new
                                           blobs are placed
           | { "$copies": n, "$of": view } any n verified copies in set
```

In placement position a view is always *and*: `a | b` (view union) means
both. Disjunction is spelled `$any`, never inferred.

```hjson
"$blob_storage": {
  "$in":    "/stores['s3'] | /stores['archive']",
  "$serve": "/stores['s3']"
}
```

```hjson
"$in": { "$any": [ "/stores['new']", "/stores['old']" ] }
```

```hjson
"$in": { "$all": [
  { "$any": [ "^.storage.stores[:s | s.enabled]", "/stores['main']" ] },
  { "$copies": 2, "$of": "/stores[:s | s.region == 'eu']" }
] }
```

Semantics:

```text
satisfaction   the boolean evaluation of the tree over the blob's
               verified locations ($stored, §5)
new placement  the leftmost plan: $all places every branch, $any its
               first alternative, $copies the first n of the set in
               view order, a view all its stores
tolerance      a blob satisfying ANY alternative rests where it is —
               grandfathering is written in the requirement, never
               implied by an absent key; contrast the two spellings:
               { $any: [new, old] }  old placements tolerated
               new ?? old            old leaves the requirement when new
                                     exists: existing blobs converge
surplus        a verified copy in a store the requirement never mentions
               drains after the store's retention grace
drain a store  remove it from the requirements that mention it
```

## 4. Reconciliation

Requirements are fold-deterministic functions of data, so change
detection is free: the commit that changed a requirement's inputs is the
change event. A background reconciler converges actual toward required:

```text
copy       from any verified holder, sha512-checked at destination
           before the copy counts; idempotent (content addressing),
           resumable, throttled by engine budgets — long migrations are
           expected and safe
serve      reads never blink: any verified holder serves throughout,
           preferring $serve order
status     every reconciler observation is a system-actor commit into
           the blob's placement status (§5) — dashboards are ordinary
           views; status is engine-writable only, no surface can forge it
```

## 5. The blob meta API

A blob value `b` exposes three groups — all model-visible, typed, usable
in views, checks, `$order`, and meters.

Content (immutable, set at ingress):

```text
b.$sha512   text    b.$bytes   int    b.$media   text    b.$name   text?
```

Policy (resolved requirement, deterministic):

```text
b.$policy.$in      the placement tree, resolved
b.$policy.$serve   view<stores>, read order
```

Placement (real location, as data — reconciler commits):

```text
b.$placement                     keyed by store
b.$placement[s].$state           $enum: pending | copying | verified
                                        | corrupt | draining
b.$placement[s].$since           when the state was entered
b.$placement[s].$checked         last sha512 verification
b.$placement[s].$progress        int?, bytes landed while copying

b.$stored      view<stores>      placement[:p | p.$state == 'verified']
b.$satisfied   bool              $in evaluated over $stored
b.$surplus     view<stores>      $stored minus stores the requirement
                                 mentions
```

`corrupt` demotes the copy — `$stored` stays truthful, re-copy is
triggered from a verified holder; the descriptor is never demoted: history
is correct by definition, storage must prove it holds it.

Billing on **real location** is therefore an ordinary view:

```hjson
"billing": "= /companies:: { company: companies.$key, s3_gb: sum(companies.uploads[:u | /stores['s3'] in u.file.$stored].file.$bytes) / GB, pending: count(companies.uploads[:u | !u.file.$satisfied]) }"
```

Deterministic at any frontier, replayable, closable through a meter for
period invoicing. A migrating blob does not bill for a store until the
verified commit lands. `$placement` is data, hence deterministic — what it
is not is instantaneously synchronized with disks; the gap is reconciler
freshness, spot-checked by the physical API (`store.usage`, `$accuracy`
tags), which is a verification tool, never a billing source.

## 6. Integrity: sha512 end to end

```text
ingress     hashed as bytes stream; size capped at $max_bytes as they
            arrive; media validated; descriptor issued only on match
presigned   direct-to-bucket PUTs verified before the commit gate:
            size always, sha512 via connector checksum where supported,
            engine re-read otherwise
migration   re-hashed at destination before a copy counts
scrub       periodic re-verification, engine policy; failures demote
            copies and trigger re-copy
store keys  objects are keyed by $sha512 — dedup, verification, and
            copy-idempotence are one mechanism
```

## 7. The commit gate and atomicity

A mutation with blob parameters is one transactional outcome (CLIENT.md):
bytes land in the leftmost plan's first store, are verified, the
descriptor binds to `@param`, the mutation applies — or no commit is admitted. A
descriptor is referenceable once at least one verified copy exists within
the requirement; full satisfaction is reconciliation's job, visible in
`$satisfied`. Disconnect or cancel mid-stream leaves no descriptor and no
commit; orphan bytes sweep after a TTL.

## 8. Retention and vacuum

Descriptors in history pin bytes by default (retention = infinite). Per
store, declare retention to allow reclamation:

```text
blobs.vacuum(before_frontier)   drops bytes only for descriptors
                                unreachable from live data AND outside
                                the store's retention window; history
                                keeps tombstoned descriptors — hash
                                remains, integrity stays checkable
```

Surplus drains obey the same per-store grace.

## Reference card

```text
descriptor   { $sha512, $bytes, $media, $name? }; bytes outside the fold
acceptance   blob params declare $max_bytes and $media, enforced streaming
stores       connector = capability; store = row; vault refs for secrets
requirement  $blob_storage.$in — view (and) | $all | $any (ordered,
             first = new placement) | $copies n $of set; $serve = read order
tolerance    written as $any alternatives; ?? converges — always a
             visible spelling, never an absent-key mode
meta         b.$sha512/$bytes/$media · b.$policy.$in/$serve ·
             b.$placement[s].$state/$since/$checked/$progress ·
             b.$stored · b.$satisfied · b.$surplus
billing      views over $stored — real, verified location, deterministic
integrity    sha512 at ingress, presign, migration, scrub; mismatch
             demotes the copy, never the descriptor
gate         one verified copy within the requirement admits the commit
retention    per store; vacuum tombstones content, keeps hashes
```
