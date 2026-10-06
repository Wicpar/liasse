# Liasse Builtins

The core library contracts of `liasse 0.0.1`, supplied through host libraries
(LANGUAGE 7.4). Values, operators, constraints, and module composition have the
contracts below on every backend. The further library functions listed here
have these contracts when provided; a destination may need a shim for them.
A builtin can be opaque or defined in Liasse, as the templates of section 6 are.
[LANGUAGE.md](LANGUAGE.md) defines the syntax and language semantics;
[SPEC.md](SPEC.md) defines system guarantees and the archive format.

Each builtin is given by its signature, written as the signature of an opaque
(LANGUAGE 7.4). A signature that starts with `mut` declares a mutation
(LANGUAGE 5), the other signatures of section 4 declare functions, and those
of section 1.3 declare operators (LANGUAGE 7.4).

---

## 1. Host types

| Type | Values | Literal or text representation |
| --- | --- | --- |
| `Bool` | `false` and `true` | `true`, `false` |
| `Int` | the integers, of any size | integers: `-7` |
| `Decimal` | the exact decimal numbers | decimals and integers: `3.50`, `3` |
| `Text` | the sequences of Unicode scalar values | texts: `"Paris"` |
| `Bytes` | the byte strings | texts of two hexadecimal digits per byte: `"0aff"` |
| `Uuid` | the 128-bit UUIDs of RFC 9562 | texts in the 8-4-4-4-12 hexadecimal form of RFC 9562 |
| `Date` | the dates of the proleptic Gregorian calendar | texts in the RFC 3339 full-date form: `"2026-09-25"` |
| `Timestamp` | the instants, as signed counts of microseconds since `1970-01-01T00:00:00Z` | texts in the RFC 3339 date-time form, with `Z` or an offset |
| `Duration` | the elapsed times, as signed counts of microseconds | texts in the ISO 8601 form with day and time components: `"P1DT2H"` |
| `Blob(storage: Text = "default")` | the descriptors of binary content (§1.4) | |

Only `Bool`, `Int`, `Decimal`, and `Text` have source literals. The other
text forms require an explicit constructor or conversion.

`Bool`, `Int`, `Decimal`, `Text`, `Bytes`, `Uuid`, `Date`, `Timestamp`, and
`Duration` are **key-eligible**, and so are a refinement of a key-eligible type
and an object whose members are all required and key-eligible.

- `Text`: its scalar values are the code points U+0000 to U+D7FF and U+E000 to
  U+10FFFF, U+0000 included. A text is kept exactly as written: a precomposed
  character and its combining sequence are two different texts.
- `Decimal`: a decimal is its numeric value: `1.5` and `1.50` are one value. Its
  **minimal-scale form** is the pair `[e, m]` of an exponent and a mantissa not
  divisible by 10 whose value is `m × 10^e`, with zero as `[0, 0]`: `1.5` is
  `[-1, 15]`, and one hundred is `[2, 1]`. Its text, in diagnostics, is its
  minimal-scale positional form: every digit written out, trailing fractional
  zeros and a trailing decimal point removed, a leading `-` for a negative
  value, and `0` for zero.

### 1.1 Order

The values of each type have one total order, which depends on the values alone.
Values of different types compare where the language compares them: a refined
value with its base type, and two sets of one element type, whatever their
cardinalities.

| Type | Order |
| --- | --- |
| `Bool` | `false`, then `true` |
| `Int` | the order of the integers |
| `Decimal` | the order of the numbers; numerically equal values are equal |
| `Text` | lexicographic order of the scalar values, which is the bytewise order of their UTF-8 encodings; a proper prefix first |
| `Bytes` | lexicographic order of the unsigned bytes; a proper prefix first |
| `Uuid` | lexicographic order of its 16 bytes, unsigned |
| `Date` | chronological order |
| `Timestamp` | chronological order |
| `Duration` | signed length: negative durations, then zero, then positive durations |
| `Blob` | `digest`, then `size` |
| an optional value | missing before present; present values compare by their inner type |
| a set | its elements in their existing order, compared pair by pair: the first pair that differs decides, and a proper prefix comes first; `[]` comes before every nonempty set |
| a record, an object | lexicographic over its members, in the ASCII order of their names |

Texts compare by their scalar values as written: `"Zeta"` comes before
`"alpha"`, and `"10"` before `"9"`.

### 1.2 Limits

These limits hold for every value, wherever it is written, computed, or received:

| Value | Limit |
| --- | --- |
| `Decimal` | the exponent of its minimal-scale form lies between −16383 and 16383 |
| `Date` | from `0000-01-01` to `9999-12-31` |
| `Timestamp` | from `0000-01-01T00:00:00Z` up to `10000-01-01T00:00:00Z`, excluded |
| `Duration` | a magnitude below 3 652 425 days |

### 1.3 Operators

These are the operator declarations (LANGUAGE 7.4):

| Declaration | Description |
| --- | --- |
| `fn Int + Int: Int`, `fn Decimal + Decimal: Decimal` | The sum. |
| `fn Int - Int: Int`, `fn Decimal - Decimal: Decimal` | The difference. |
| `fn Int * Int: Int`, `fn Decimal * Decimal: Decimal` | The product. |
| `fn Int / Int: Int` | The quotient, truncated toward zero. |
| `fn Decimal / Decimal: Decimal` | The quotient. |
| `fn Int % Int: Int`, `fn Decimal % Decimal: Decimal` | The remainder. |
| `fn -Int: Int`, `fn -Decimal: Decimal`, `fn -Duration: Duration` | The negation. |
| `fn !Bool: Bool` | The negation. |
| `fn Text + Text: Text`, `fn Bytes + Bytes: Bytes` | The concatenation. |
| `fn Timestamp + Duration: Timestamp`, `fn Timestamp - Duration: Timestamp` | The instant later or earlier by the right operand, exact in microseconds. |
| `fn Timestamp - Timestamp: Duration` | The time elapsed since the right operand. |
| `fn Duration + Duration: Duration`, `fn Duration - Duration: Duration` | The sum and the difference. |
| `fn<V> V == V: Bool`, `fn<V> V != V: Bool` | Equality by value (LANGUAGE 3.2). |
| `fn<V> V < V: Bool`, `fn<V> V <= V: Bool`, `fn<V> V > V: Bool`, `fn<V> V >= V: Bool` | The order of 1.1. |
| `fn<V> V in V*: Bool` | The right operand has an element equal to the left one (LANGUAGE 3.2). |

`+`, `-`, and `*` on `Int` and `Decimal` give the exact result.

- `Int / Int` truncates toward zero: `7 / 2` is `3`, and `-7 / 2` is `-3`.
- `Decimal / Decimal` gives a terminating quotient exactly: `10.0 / 4` is `2.5`.
  A non-terminating quotient is rounded half away from zero to 20 fractional
  digits, or to 16 significant digits when that keeps more fractional digits:
  `1 / 3.0` is `0.33333333333333333333`, `2 / 3.0` is `0.66666666666666666667`,
  and `1 / 300000.0` is `0.000003333333333333333`.
- `a % b` is `a − trunc(a ÷ b) × b`, with `a ÷ b` the exact quotient, and takes
  the sign of `a`: `-7 % 2` is `-1`, `7 % -2` is `1`, and `5.5 % 2` is `1.5`.
- A zero divisor of `/` or `%` rejects the evaluation with the code
  `evaluation`.

### 1.4 Blobs

A `Blob` names content by `digest: Bytes`, its 64-byte SHA-512 digest, and
`size: Int`, its nonnegative byte length. Invalid descriptors or content that
differs from its descriptor are malformed. Content MUST be verified before it
is accepted or delivered. A transaction storing a descriptor commits only once
its content is durable; the content is retained while state or retained history
reaches it.

The `storage` argument is a name that the host binds to storage. The backend
chooses locations, replication, and transfer methods. The name affects placement,
while equality and exports depend on the descriptor and content alone.

---

## 2. Templates

| Signature | Description |
| --- | --- |
| `Map<K, V>` | A stored set of keys, each unique, with one value each (§6). |
| `Enum(labels: Text*)` | A `Text` restricted to `labels`, in the order of `Text`, whatever the order of `labels`. |
| `Module(target: Text? = none)` | A mount point whose child satisfies its declared interface (LANGUAGE 7.3). |
| `Parent` | The interface that the child's mount point provides (LANGUAGE 7.3). |

### 2.1 Instances

The call `modules.init(artifact)` initializes the package's current definition
from its sources, assigns the new instance a fresh UUID, and returns it for
assignment to a mount point. It uses the declaration's initializers; importing
an archive's recorded instances is the separate operation in SPEC §2.1.
A mount point already holding an instance rejects the assignment. A required
mount point must be filled in the transaction that constructs its owner.

Assigning `none` to an optional mount point, removing its owner, or removing it
in a migration unmounts the child and its descendants atomically. Their state
remains in retained history. Access through the removed mount ends at commit.
An instance has one owner. An application's root requires no `Parent` context.

The host validates the resulting state and both interfaces before mounting or
updating. Reads through `Parent` use the child's snapshot and instant; provided
mutations run at the calling statement in the shared transaction. The language's
acyclic-call rule includes calls across these interfaces.

### 2.2 Versions and updates

Packages supplied for one module name must agree on the files of every shared
version and migration. A divergent module takes another name. An instance's
**target** is, in order of precedence, its `Module(target: "name@version")`,
the current version last supplied to it by `modules.update`, its module-wide
target supplied by the host, or its active version. A mount's target is part of
its stored structure; changing it requires a migration.

The call `modules.update(m, artifact)` gives a package to the instance at `m`.
An empty mount, a mount with a fixed target, or a package of another module
rejects the call. Giving a package sets a target; the update moves the instance
in a separate atomic commit. It traverses consecutive versions toward the
target, applying LANGUAGE 8, or carrying state unchanged across an equal
structure. The active structure must agree with the route's starting version.

An instance moves as far toward its target as its migrations, constraints, and
interfaces allow. Instances whose new interfaces require each other move
together. In such an update, a parent's final migration programs run after its
children's routes, which run after the rest of the parent's route. All results
are validated and committed together.

A blocked instance keeps running its active version. The host reports the
failing checks and retries when relevant state, definitions, or targets change.
It may bound the search and report `contention`. Scheduling and parallelism are
backend choices.

---

## 3. Mixins

| Signature | Description |
| --- | --- |
| `assert(condition: Bool, message: Text? = none)` | Checks `condition` at the point specified by LANGUAGE 2.5: failure gives `constraint` in a type body or `assertion` in a program. An omitted message is generated from the condition. |
| `unique<V, K>(key: V -> K = .)` | At a set level, requires distinct present keys among one owner's rows or an argument's elements. Failure gives `uniqueness`, naming the set, key, and conflicting value. In replacement, a new element's key matches an existing row; keys matching different rows reject the transaction (LANGUAGE 5.2). |

- A body `assert` reads what a view reads.
- `key` reads the data members of the row or the element alone, makes immutable
  calls (LANGUAGE 4.4), and gives a value of cardinality one or `?`: of a
  key-eligible type (§1), or a record of such values.
  A missing key, or a record key with any missing component, is excluded from
  uniqueness checking and replacement matching. Every other key is compared by
  value, including an empty text or byte string.
- A selection of a stored set read from its owner has the cardinality `?`
  (LANGUAGE 3.3) when its equalities cover the `key` of one `unique` of that
  stored set, and that key is a member of the row, a record of
  members, or `.` for rows that are scalars:
  `.attributes[.key == "color"].value ?? "plain"`.

---

## 4. Functions and mutations

| Signature | Description |
| --- | --- |
| `size(x: Text): Int` | The length in Unicode scalar values. |
| `round(x: Decimal, digits: Int = 0): Decimal` | Rounded to `digits` fractional digits, a halfway value away from zero. |
| `trunc(x: Decimal, digits: Int = 0): Decimal` | Rounded toward zero to `digits` fractional digits. |
| `floor(x: Decimal): Decimal` | The greatest integer at most `x`. |
| `ceil(x: Decimal): Decimal` | The least integer at least `x`. |
| `now(): Timestamp` | The fixed instant of the transaction or read (SPEC §1.1); a stable call. |
| `mut uuid(): Uuid` | A fresh UUID at each call. |

A negative `digits` rounds left of the decimal point. `round(2.5)` is `3`,
`round(-2.5)` is `-3`, `round(1.245, 2)` is `1.25`, and `round(1250, -2)` is
`1300`. `trunc(-2.7)` is `-2`, and `trunc(2.789, 1)` is `2.7`. `floor(-2.5)` is
`-3`, and `ceil(-2.5)` is `-2`.

| Namespace | Publishes |
| --- | --- |
| `string` | Unicode text functions (4.1). |
| `convert` | Checked conversions (4.1). |
| `bytes` | `bytes.size(x: Bytes): Int`: the length in bytes. |
| `sha` | Cryptographic hashes: `sha.sha256(data: Bytes): Bytes`, `sha.sha384(data: Bytes): Bytes`, and `sha.sha512(data: Bytes): Bytes`, the SHA-2 digests of `data` (FIPS 180-4). |
| `random` | `mut random.bytes(n: Int): Bytes`: `n` fresh bytes at each call, from a cryptographically secure source. A negative `n` rejects the evaluation. |
| `crypto` | Signatures: Ed25519, and ECDSA over P-256 and P-384 (4.2). |
| `history` | `mut history.at<V>(source: V*, instant: Timestamp): { row: V, history: { created_at: Timestamp, updated_at: Timestamp } }*`: past rows and their metadata (§4.3); a stable call. |
| `modules` | `mut modules.init(artifact: Blob): Module` and `mut modules.update(m: Module?, artifact: Blob)`: initialization and update (§2); state-changing calls. |

### 4.1 Text and conversions

`liasse 0.0.1` follows Unicode 16.0.0: each `string` function gives the result
that the character properties and case mappings of that version define.

| Signature | Description |
| --- | --- |
| `string.trim(value: Text): Text` | `value` with its leading and trailing scalar values of the property `White_Space` removed. |
| `string.lower(value: Text): Text` | The default case conversion `toLowercase` (Unicode §3.13). |
| `string.upper(value: Text): Text` | The default case conversion `toUppercase` (Unicode §3.13). |
| `string.casefold(value: Text): Text` | The default case folding `toCasefold` (Unicode §3.13). |
| `string.starts_with(value: Text, prefix: Text): Bool` | `value` begins with the scalar values of `prefix`. |
| `convert.int_text(value: Int): Text` | The base-10 text of `value`: `0` for zero, the digits from the first nonzero digit otherwise, after one `-` for a negative value. |
| `convert.int_decimal(value: Int): Decimal` | The `Decimal` equal to `value`. |
| `convert.uuid_text(value: Uuid): Text` | The 32 lower-case hexadecimal digits of the 16 bytes of `value`, in the 8-4-4-4-12 grouping of RFC 9562, separated by `-`. |

### 4.2 Signatures

Every verification gives `false` for every invalid signature, a malformed key or
signature included. A signature under a malformed private key rejects the
evaluation with the code `evaluation`.

| Signature | Description |
| --- | --- |
| `crypto.ed25519_verify(public_key: Bytes, message: Bytes, signature: Bytes): Bool` | `signature` is an Ed25519 signature (RFC 8032) of `message` under `public_key`, by the verification of RFC 8032 §5.1.7 with these checks: `S` below the group order, and `public_key` and `R` in canonical encoding and not of small order. |
| `mut crypto.ed25519_generate(): { public_key: Bytes, private_key: Bytes }` | A new Ed25519 key pair, from a cryptographically secure source: the 32-byte public key and the 32-byte private key. |
| `crypto.ed25519_sign(private_key: Bytes, message: Bytes): Bytes` | The 64-byte Ed25519 signature of `message` under `private_key` (RFC 8032 §5.1.6). |
| `crypto.p256_verify(public_key: Bytes, message: Bytes, signature: Bytes): Bool` | `signature` is an ECDSA signature (FIPS 186-5 §6.4.2) over the curve P-256 of the SHA-256 digest of `message` under `public_key`. |
| `mut crypto.p256_generate(): { public_key: Bytes, private_key: Bytes }` | A new P-256 key pair, from a cryptographically secure source: the 65-byte uncompressed public key and the 32-byte private key. |
| `crypto.p256_sign(private_key: Bytes, message: Bytes): Bytes` | The 64-byte ECDSA P-256 signature of the SHA-256 digest of `message` under `private_key`, with the nonce of RFC 6979. |
| `crypto.p384_verify(public_key: Bytes, message: Bytes, signature: Bytes): Bool` | `signature` is an ECDSA signature (FIPS 186-5 §6.4.2) over the curve P-384 of the SHA-384 digest of `message` under `public_key`. |
| `mut crypto.p384_generate(): { public_key: Bytes, private_key: Bytes }` | A new P-384 key pair, from a cryptographically secure source: the 97-byte uncompressed public key and the 48-byte private key. |
| `crypto.p384_sign(private_key: Bytes, message: Bytes): Bytes` | The 96-byte ECDSA P-384 signature of the SHA-384 digest of `message` under `private_key`, with the nonce of RFC 6979. |

- **Private keys**: an Ed25519 private key is its 32-byte seed (RFC 8032
  §5.1.5), and an ECDSA private key its scalar, big-endian in 32 bytes for P-256
  and 48 bytes for P-384, from 1 to the group order minus 1.
- **ECDSA public keys** are points in the encoding of SEC 1 §2.3.3:
  uncompressed, as `0x04` then the two coordinates, or compressed, as `0x02` or
  `0x03` then the first coordinate, each coordinate big-endian in 32 bytes for
  P-256 and 48 bytes for P-384. A verification takes both forms, and a key off
  the curve is malformed. A generation gives the uncompressed form.
- **ECDSA signatures** are `r` then `s`, each big-endian in 32 bytes for P-256
  and 48 bytes for P-384, both from 1 to the group order minus 1: the form of
  IEEE P1363, WebCrypto, JOSE, and COSE. Whenever `r ‖ s` verifies, so does
  `r ‖ (n − s)`, with `n` the group order: two signature values sign one
  message.

---

### 4.3 Past rows

The call `history.at(source, instant)` reads the calling instance's retained
history within its transaction's snapshot. The state at an instant includes the
commits in that history before the first whose instant is later. A request
needing history omitted by a current-state import fails with `evaluation`.
Within retained history, a set that had yet to be constructed contributes no rows.

The source is a stored set of the calling instance or a selection of its rows.
For a whole set, the result contains all its rows then, including rows since
removed. For a selection, it contains the selected rows as they stood then.
Reusing a removed row's text identity does not make it the same historical row.
Past state is carried through migrations to the active version; the result
contains rows whose carried values supply every required member, in their
historical order, with their metadata then.

Past rows are values with their original origins, so replacement (LANGUAGE 5.2)
can update surviving rows, insert removed rows anew, and remove rows absent from
the supplied result. Surviving rows keep their current order; reinserted rows
follow them.

## 5. Metaproperties

| Metaproperty | Type | Description |
| --- | --- | --- |
| `$history` | `{ created_at: Timestamp, updated_at: Timestamp }` | The instants of the row's insertion and last change to its own stored members. |

Insertion sets both instants to the commit's instant. A change to the row's
own value or stored object members sets `updated_at` to that commit's instant.
Changes inside a nested stored set, equal-value writes, and per-owner migration
transformations preserve both timestamps. A per-owner transformation reads and
writes only one row or one owner's object: structural changes, local
initializers, retype mappings, and scoped programs with that restriction.
Other migration writes follow the ordinary write rule. A rename preserves the
row and its metadata.
Wall-clock adjustments can make `updated_at` earlier than `created_at`.

---

## 6. Template definitions

`Map` and `Enum` are these templates:

```liasse
/// Keys, each unique, with one value each.
template Map<K, V>: {
    key: K
    value: V
}* { unique(.key) }

/// A `Text` restricted to `labels`.
template Enum(labels: Text*): Text {
    assert(. in labels, "Unknown label")
}
```

`x in s` tests membership in a set, and `k in m.key` a key of a map.
