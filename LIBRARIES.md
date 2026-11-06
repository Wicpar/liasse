# Alpha Host Libraries

The host libraries (LANGUAGE 7.4) that the alpha supplies to every module of
`liasse 0.0.1`, beyond the builtins of [SPEC.md](SPEC.md) chapter 2, and the
keyring library, which the host gives to the modules that it chooses (§3.7).
The libraries of sections 3.4 to 3.6 also serve clients (CONNECT 2.1). Each
declaration has the contract below wherever an implementation supplies it.
A declaration can be opaque or defined in Liasse, as the templates of section 5
are. [LANGUAGE.md](LANGUAGE.md) defines the syntax and language semantics.

Each declaration is given by its signature, written as the signature of an
opaque (LANGUAGE 7.4). A signature that starts with `mut` declares a mutation
(LANGUAGE 5), the other signatures of section 3 declare functions, and those
of section 1.3 declare operators (LANGUAGE 7.4).

---

## 1. Host types

These host types extend the builtin types of SPEC chapter 2. Their values
reach `data.cbor` and `history.cbor` as SPEC §4.1 wraps a host type's value,
with the representation below:

| Type | Values | Text form | CBOR representation |
| --- | --- | --- | --- |
| `Bytes` | the byte strings | two hexadecimal digits per byte: `"0aff"` | a byte string |
| `Uuid` | the 128-bit UUIDs of RFC 9562 | the 8-4-4-4-12 hexadecimal form of RFC 9562 | tag 37 over its 16 bytes |
| `Date` | the dates of the proleptic Gregorian calendar | the RFC 3339 full-date form: `"2026-09-25"` | tag 1004 over its text form |
| `Timestamp` | the instants, as signed counts of microseconds since `1970-01-01T00:00:00Z` | the RFC 3339 date-time form, with `Z` or an offset | a signed integer |
| `Duration` | the elapsed times, as signed counts of microseconds | the ISO 8601 form with day and time components: `"P1DT2H"` | a signed integer |

The conversions of §3.1, and `hex` (§3.6) for `Bytes`, read and write the text
forms.

`Bool`, `I64`, `U64`, `Text`, `Bytes`, `Uuid`, `Date`, `Timestamp`, and
`Duration` are **key-eligible**, and so are a refinement of a key-eligible type
and an object whose members are all required and key-eligible.

### 1.1 Order

The values of each type have one total order, which depends on the values alone:
the order of SPEC §2.1 for `Bool`, `I64`, `U64`, and `Text`, and the order
below for the others. Values of different types compare where the language
compares them: a refined value with its base type, and two sets of one element
type, whatever their cardinalities.

| Type | Order |
| --- | --- |
| `Bytes` | lexicographic order of the unsigned bytes; a proper prefix first |
| `Uuid` | lexicographic order of its 16 bytes, unsigned |
| `Date` | chronological order |
| `Timestamp` | chronological order |
| `Duration` | signed length: negative durations, then zero, then positive durations |
| `Blob` | its digest bytewise, then its size |
| an optional value | missing before present; present values compare by their inner type |
| a set | its elements in their existing order, compared pair by pair: the first pair that differs decides, and a proper prefix comes first; `[]` comes before every nonempty set |
| a record, an object | lexicographic over its members, in the ASCII order of their names |

### 1.2 Limits

These limits hold for every value, wherever it is written, computed, or received:

| Value | Limit |
| --- | --- |
| `Date` | from `0000-01-01` to `9999-12-31` |
| `Timestamp` | from `0000-01-01T00:00:00Z` up to `10000-01-01T00:00:00Z`, excluded |
| `Duration` | a magnitude below 3 652 425 days |

### 1.3 Operators

These operator declarations (LANGUAGE 7.4) extend those of SPEC §2.2:

| Declaration | Description |
| --- | --- |
| `fn -Duration: Duration` | The negation. |
| `fn Bytes + Bytes: Bytes` | The concatenation. |
| `fn Timestamp + Duration: Timestamp`, `fn Timestamp - Duration: Timestamp` | The instant later or earlier by the right operand, exact in microseconds. |
| `fn Timestamp - Timestamp: Duration` | The time elapsed since the right operand. |
| `fn Duration + Duration: Duration`, `fn Duration - Duration: Duration` | The sum and the difference. |
| `fn T < T: Bool`, `fn T <= T: Bool`, `fn T > T: Bool`, `fn T >= T: Bool`, for each type `T` that 1.1 orders beyond those of SPEC §2.1 | The order of 1.1. |

---

## 2. Templates

| Signature | Description |
| --- | --- |
| `Map<K, V>` | A stored set of keys, each unique, with one value each (§5). |
| `Enum(labels: Text*)` | A `Text` restricted to `labels`, in the order of `Text`, whatever the order of `labels`. |

### 2.1 Instances

The call `modules.init(artifact)` initializes the package's current definition
from its sources, assigns the new instance a fresh UUID, and returns it for
assignment to a mount point. It uses the declaration's initializers; importing
an archive's recorded instances is the separate operation of SPEC §3.3.
Assigning an instance to a mount point mounts it as SPEC §1.1 states,
preserving the owner's lifetime. A required mount stays filled through replacement and must be filled in the
transaction that constructs its owner. Host import chooses empty destination
mounts (ALPHA, Lifecycle and transfer).

The state of an unmounted instance remains in retained history. Access
through the removed mount ends at commit. An application's root requires no
`Parent` context.

The host validates the resulting state and both interfaces before mounting or
updating. Reads through `Parent` use the child's current read context;
provided mutations contribute their operations at the calling
statement in the shared transaction. Ordinary view reads use their snapshot.
The language's acyclic-call rule includes calls across these interfaces.

### 2.2 Versions and updates

Packages supplied for one module name agree as SPEC §3.1 requires. An
instance's **target** (SPEC §1.3) is, in order of precedence, the version that
its mount point pins (LANGUAGE 7.3), the current version last supplied to it by
`modules.update`, its module-wide target supplied by the host, or its active
version. A pin is part of its mount point's stored structure; changing it
requires a migration.

The call `modules.update(m, artifact)` gives a package to the instance at `m`.
An empty mount, a pinned mount, or a package of another module
rejects the call. Giving a package sets a target; the instance moves toward it
(SPEC §1.3) in a transaction after the supplying transaction commits. The
active structure must agree with the route's starting version.

The host reports the failing checks of an instance stopped short of its target
(SPEC §1.3).

---

## 3. Functions and mutations

| Signature | Description |
| --- | --- |
| `size(x: Text): U64` | The length in Unicode scalar values. |
| `now(): Timestamp` | An instant consistent with the current time, constant throughout the transaction or read; a stable call. |
| `mut uuid(): Uuid` | A fresh UUID at each call. |
| `skip<V>(items: V*, n: U64): items?!` | The elements after the first `n`, in order; `[]` when `items` has at most `n`. |
| `take<V>(items: V*, n: U64): items?!` | The first `n` elements, in order; every element when `items` has at most `n`. |

A retried transaction may receive a new value of `now()`.

| Namespace | Publishes |
| --- | --- |
| `string` | Unicode text functions (3.1). |
| `convert` | Checked conversions (3.1). |
| `bytes` | `bytes.size(x: Bytes): U64`: the length in bytes. |
| `sha` | Cryptographic hashes: `sha.sha256(data: Bytes): Bytes`, `sha.sha384(data: Bytes): Bytes`, and `sha.sha512(data: Bytes): Bytes`, the SHA-2 digests of `data` (FIPS 180-4). |
| `random` | `mut random.bytes(n: U64): Bytes`: `n` fresh bytes at each call; and `mut random.u64_below(bound: U64): U64`: an integer from 0 to `bound` minus 1, each equally likely, fresh at each call, where a `bound` of 0 fails the evaluation; both from a cryptographically secure source. |
| `crypto` | Signatures: Ed25519, and ECDSA over P-256 and P-384; and message authentication codes (3.2). |
| `time` | Instants, dates, durations, and time zones (3.4). |
| `hex`, `base64` | Text encodings of bytes (3.6). |
| `ecdsa`, `rsa`, `webauthn` | Signature formats, RSA signatures, and WebAuthn (3.6). |
| `keyring` | Signatures under keys that the system keyring hides (3.7). |
| `history` | `mut history.at<V>(source: V*, instant: Timestamp): { row: V, history: { created_at: Timestamp, updated_at: Timestamp } }*`: past rows and their metadata (§3.3); a stable call. |
| `modules` | `mut modules.init(artifact: Blob): Module` and `mut modules.update(m: Module?, artifact: Blob)`: initialization and update (§2); state-changing calls. |

### 3.1 Text and conversions

`liasse 0.0.1` follows Unicode 16.0.0: each `string` function gives the result
that the character properties and case mappings of that version define.

| Signature | Description |
| --- | --- |
| `string.trim(value: Text): Text` | `value` with its leading and trailing scalar values of the property `White_Space` removed. |
| `string.lower(value: Text): Text` | The default case conversion `toLowercase` (Unicode §3.13). |
| `string.upper(value: Text): Text` | The default case conversion `toUppercase` (Unicode §3.13). |
| `string.casefold(value: Text): Text` | The default case folding `toCasefold` (Unicode §3.13). |
| `string.starts_with(value: Text, prefix: Text): Bool` | `value` begins with the scalar values of `prefix`. |
| `string.ends_with(value: Text, suffix: Text): Bool` | `value` ends with the scalar values of `suffix`. |
| `string.contains(value: Text, part: Text): Bool` | The scalar values of `part` occur in `value` one after another; every text contains `""`. |
| `string.slice(value: Text, start: U64, length: U64): Text` | The scalar values of `value` from the index `start`, counting from 0, up to `length` of them; `""` when `start` is at or past the end. |
| `string.split(value: Text, separator: Text): Text+` | The texts between the occurrences of `separator` in `value`, found from the left, each after the one before: `string.split("a,b,", ",")` is `["a", "b", ""]`. An empty `separator` fails the evaluation. |
| `string.join(values: Text*, separator: Text): Text` | The elements of `values`, in order, with `separator` between each two: `string.join(["a", "b"], ", ")` is `"a, b"`. |
| `string.nfc(value: Text): Text` | The Normalization Form C of `value` (UAX #15), under which canonically equivalent texts are equal. |
| `string.nfkc_casefold(value: Text): Text` | `toNFKC_Casefold(value)` (Unicode §3.13), under which texts that differ by case, compatibility forms, or default-ignorable characters are equal: the form for identifiers such as user names in a unique key. |
| `convert.i64_text(value: I64): Text`, `convert.u64_text(value: U64): Text` | The base-10 text of `value`: `0` for zero, the digits from the first nonzero digit otherwise, after one `-` for a negative value. |
| `convert.i64_u64(value: I64): U64`, `convert.u64_i64(value: U64): I64` | `value`; a value outside the result type fails the evaluation. |
| `convert.text_i64(value: Text): I64?`, `convert.text_u64(value: Text): U64?` | The integer that `value` writes in base 10: one optional `-`, for `I64` alone, then one or more ASCII digits; `none` for any other text, and for a value outside the result type. |
| `convert.uuid_text(value: Uuid): Text` | The 32 lower-case hexadecimal digits of the 16 bytes of `value`, in the 8-4-4-4-12 grouping of RFC 9562, separated by `-`. |
| `convert.text_uuid(value: Text): Uuid?` | The UUID that `value` writes in the 8-4-4-4-12 form of RFC 9562, with hexadecimal digits of either case; `none` for any other text. |
| `convert.text_bytes(value: Text): Bytes` | The UTF-8 encoding of `value`. |
| `convert.bytes_text(value: Bytes): Text?` | The text whose UTF-8 encoding is `value`; `none` for malformed UTF-8. |
| `convert.date_text(value: Date): Text` | The text form of `value` (§1): `YYYY-MM-DD`. |
| `convert.text_date(value: Text): Date?` | The date that the text form `value` names; `none` for any other text, and for a date outside §1.2. |
| `convert.timestamp_text(value: Timestamp): Text` | The text form of `value` in UTC: `YYYY-MM-DDThh:mm:ss`, then `.` and six digits when its microseconds are nonzero, then `Z`. |
| `convert.text_timestamp(value: Text): Timestamp?` | The instant that the RFC 3339 date-time `value` names, with `Z` or an offset and up to six fractional digits; `none` for any other text, a second of 60, and an instant outside §1.2. |
| `convert.duration_text(value: Duration): Text` | The text form of `value`: `-` for a negative duration, then `P`; the days and `D` when they are nonzero; then, when hours, minutes, or seconds remain, `T` and each nonzero one with `H`, `M`, or `S`, the seconds with six fractional digits when the microseconds are nonzero. A day is 24 hours: `P1DT2H`, `PT0.500000S`, and `PT0S` for zero. |
| `convert.text_duration(value: Text): Duration?` | The duration that `value` writes in that form, with any of its components and up to six fractional digits of seconds; `none` for any other text, and for a magnitude outside §1.2. |

### 3.2 Signatures and message authentication

Every verification gives `false` for every invalid signature, a malformed key or
signature included. A signature under a malformed private key fails the
evaluation.

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
| `crypto.hmac_sha256(key: Bytes, message: Bytes): Bytes` | The 32-byte HMAC (RFC 2104) of `message` under `key`, with SHA-256. |
| `crypto.hmac_sha512(key: Bytes, message: Bytes): Bytes` | The 64-byte HMAC of `message` under `key`, with SHA-512. |

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

### 3.3 Past rows

The call `history.at(source, instant)` reads the calling instance's retained
history within its current read context. In a committing builder this is the
history prefix preceding its commit. A materialized preview is an observation
to validate before commit. The state at an instant includes the commits in
that history before the first whose instant is later. A request needing history
from before a data-only import (SPEC §3.3) fails with `evaluation`.
Within retained history, a set that had yet to be constructed contributes no rows.

The source is a stored set of the calling instance or a selection of its rows.
For a whole set, the result contains all its rows then, including rows since
removed. For a selection, it contains the selected rows as they stood then.
Reinserting a removed row starts a new lifetime.
An empty historical selection returns `[]`.
Version adaptation applies the changes of each later move to a private copy of
the complete historical instance:

1. Replay the history up to that state, and remember the queried rows by their
   numbers (SPEC §5.3), with their order and timestamps.
2. For each later record that moves the version, in commit order, apply its
   changes to the private copy. Renames, drops, filters, and computed values
   reach every row of the copy; a change to a numbered row reaches the copy's
   row of that number, when it has one. After each record, select its version
   and validate the copy's stored shape.
3. Return the remembered rows that the copy still holds, with their adapted
   values, order, and timestamps.

Adaptation runs zero initializers, mutation bodies, migration programs, drawn
calls, or foreign-instance calls, and changes no live state. A row that a move
leaves without a required value, and a shape that the version rejects, fail the
read with `evaluation`: a value that a move gave to numbered rows reaches those
rows alone, so a row removed before the move takes no such value.

For a `1.0.0 → 2.0.0 → 1.0.0 → 2.0.0` history, adaptation applies all three
recorded moves at their respective instants. A read just after the first move
applies the last two. Ordinary commits between them are skipped.

Past rows are computed values (LANGUAGE 4.5): writing one back inserts a copy,
or puts it by a key.

### 3.4 Time

A time zone is an IANA name that the host's time zone database holds; another
name fails the evaluation.

| Signature | Description |
| --- | --- |
| `time.timestamp_to_unix(t: Timestamp): I64` | The whole seconds from `1970-01-01T00:00:00Z` to `t`, rounded toward negative infinity. |
| `time.unix_to_timestamp(seconds: I64): Timestamp` | The instant `seconds` after `1970-01-01T00:00:00Z`. |
| `time.timestamp_to_date(t: Timestamp): Date` | The date of `t` in UTC. |
| `time.date_to_timestamp(date: Date): Timestamp` | The instant at which `date` starts in UTC. |
| `time.date_add_days(date: Date, days: I64): Date` | The date `days` days after `date`, or before it for a negative `days`. |
| `time.duration_to_seconds(d: Duration): I64` | The whole seconds of `d`, rounded toward zero. |
| `time.seconds_to_duration(seconds: I64): Duration` | The duration of `seconds` seconds. |
| `mut time.timestamp_to_local_date(t: Timestamp, zone: Text): Date` | The date of `t` in the time zone `zone`, read from the host's time zone database. |
| `mut time.local_date_to_timestamp(date: Date, zone: Text): Timestamp` | The instant at which `date` starts in the time zone `zone`, read from the host's time zone database. |

### 3.5 Set functions

These functions read sets. Each keeps the order of the elements that it reads
(LANGUAGE 3.3), save `sort`, and a read gives the elements of its result in
their order.

| Signature | Description |
| --- | --- |
| `filter<V>(items: V*, keep: V -> Bool): items?!` | The elements for which `keep` holds: the selection `items[keep(.)]`. |
| `group<V, K, R>(items: V*, key: V -> K, each: { key: K, items: V+ } -> R): items \|> R` | One element for each distinct `key`, in the order of the first element with that key: the value of `each` for the record of that key and its elements: `group(.customer) { customer: .key, total: sum(.items.total) }`. |
| `distinct<V, A>(items: V*, by: V -> A = .): items` | One element of each group of elements of equal `by`, the first: `distinct(.id)`. |
| `union<V, A>(items: V*, other: V*, by: V -> A = .): V*` | The elements, then the elements of `other` whose `by` equals that of none of them. |
| `intersection<V, A>(items: V*, other: V*, by: V -> A = .): items?!` | The elements whose `by` equals that of an element of `other`. |
| `difference<V, A>(items: V*, other: V*, by: V -> A = .): items?!` | The elements whose `by` equals that of no element of `other`. |
| `count<V>(items: V*): U64` | The number of elements. |
| `sum<A + A: A>(items: A+): A`, `sum(items: none): none` | The elements added with `+`, in their order: `sum(.lines.amount) ?? 0` is zero for no lines. |
| `min<V>(items: V+): V`, `min(items: none): none` | The least element. |
| `max<V>(items: V+): V`, `max(items: none): none` | The greatest element. |
| `one<V>(items: V*): V` | The one element of `items`; any other number of elements fails the evaluation. |
| `sort<V, A>(items: V*, key: V -> A = .): items` | The elements, ordered by their `key` in the order of its type (§1.1), and elements of equal keys in their order. Several keys form one record key (§1.1): `sort({ created_at: desc(.created_at), id: .id })`. |
| `desc<V>(x: V): V` | `x`, as a `sort` key, or a field of a record key, that orders in reverse. |

- `filter`, `distinct`, `union`, `intersection`, and `difference` keep every
  element's data as it is; like every function, they compute their results
  (LANGUAGE 4.5). `skip` and `take` (§3) read sets as well.
- A `group` key is of a key-eligible type (§1), or a record of them:
  `.customer`, `{ day: time.timestamp_to_date(.at), .kind }`.
- `by` is written at the level of each element of the operands, which share what
  it reads.

### 3.6 Encodings and verification

| Signature | Description |
| --- | --- |
| `hex.encode(data: Bytes): Text` | The two lower-case hexadecimal digits of each byte of `data`. |
| `hex.decode(text: Text): Bytes?` | The bytes that `text` spells with two hexadecimal digits of either case for each byte; `none` for any other text. |
| `base64.encode(data: Bytes): Text` | `data` in base64 with padding (RFC 4648 §4). |
| `base64.decode(text: Text): Bytes?` | The bytes that the padded base64 text `text` spells; `none` for any other text. |
| `base64.url_encode(data: Bytes): Text` | `data` in base64url without padding (RFC 4648 §5). |
| `base64.url_decode(text: Text): Bytes?` | The bytes that the unpadded base64url text `text` spells; `none` for any other text. |
| `ecdsa.p256_from_der(der: Bytes): Bytes?` | The 64-byte P-256 signature that the DER encoding `der` of an `ECDSA-Sig-Value` (RFC 3279 §2.2.3) holds; `none` when `der` is not the DER encoding of two integers from 1 to the group order minus 1. |
| `ecdsa.p384_from_der(der: Bytes): Bytes?` | The 96-byte P-384 signature that the DER encoding `der` of an `ECDSA-Sig-Value` holds; `none` otherwise, as for P-256. |
| `rsa.pkcs1_sha256_verify(modulus: Bytes, exponent: Bytes, message: Bytes, signature: Bytes): Bool` | `signature` is an RSASSA-PKCS1-v1_5 signature (RFC 8017 §8.2.2) with SHA-256 of `message` under the public key `(modulus, exponent)`. |
| `rsa.pss_sha256_verify(modulus: Bytes, exponent: Bytes, message: Bytes, signature: Bytes): Bool` | `signature` is an RSASSA-PSS signature (RFC 8017 §8.1.2) with SHA-256, MGF1 with SHA-256, and a 32-byte salt, of `message` under the public key `(modulus, exponent)`. |
| `webauthn.credential(rp_id: Text, origins: Text*, challenge: Bytes, client_data: Bytes, attestation_object: Bytes): { credential: Bytes, public_key: Bytes, counter: U64, user_verified: Bool }?` | The credential that a WebAuthn registration creates (WebAuthn Level 3 §7.1): its credential ID, its public key as a COSE_Key, its signature counter, and whether the authenticator verified the user; `none` when the registration fails to verify. |
| `webauthn.verify(public_key: Bytes, rp_id: Text, origins: Text*, challenge: Bytes, client_data: Bytes, authenticator_data: Bytes, signature: Bytes): { counter: U64, user_verified: Bool }?` | The signature counter of a WebAuthn assertion under the credential public key `public_key` (WebAuthn Level 3 §7.2), and whether the authenticator verified the user; `none` when the assertion fails to verify. |

Every verification gives `false`, or `none` for WebAuthn, for every invalid
signature, a malformed key or signature included. The private keys, ECDSA public
keys, and ECDSA signatures are those of §3.2.

- **RSA keys** are the modulus and the public exponent as big-endian unsigned
  integers, the form of JWK and COSE: a modulus of 2048 to 4096 bits, and an odd
  exponent from 3 to 2^32 − 1. A signature has the byte length of the modulus.
- **WebAuthn** registrations and assertions verify when:
  - `client_data` is UTF-8 JSON whose `type` is `webauthn.create` for a
    registration and `webauthn.get` for an assertion, whose `challenge` is
    `challenge` in unpadded base64url, whose `origin` is one of `origins`, and
    whose `crossOrigin`, when present, is `false`;
  - the authenticator data, `authenticator_data` or the `authData` of the CBOR
    map `attestation_object`, starts with the SHA-256 digest of `rp_id` in
    UTF-8, and its flags have user presence set; a registration reads the
    attested credential data of `authData`, and passes over `fmt` and `attStmt`;
  - the credential public key is a COSE_Key (RFC 9052 §7) of the algorithm ES256
    (ECDSA over P-256 with SHA-256), EdDSA (Ed25519), or RS256
    (RSASSA-PKCS1-v1_5 with SHA-256, under the RSA keys above);
  - an assertion's `signature` signs `authenticator_data` followed by the
    SHA-256 digest of `client_data`: in DER for ES256, as authenticators give
    it, in 64 bytes for EdDSA, and in the modulus's length for RS256.

  `counter` is the signature counter of the authenticator data, and
  `user_verified` its user verification flag.

### 3.7 The keyring library

The keyring library hides private keys under the system keyring and signs with
them:

```liasse
//! Private keys hidden under the system keyring.
mut keyring.seal(key: Bytes): Bytes
mut keyring.ed25519_sign(sealed_key: Bytes, message: Bytes): Bytes
mut keyring.p256_sign(sealed_key: Bytes, message: Bytes): Bytes
mut keyring.p384_sign(sealed_key: Bytes, message: Bytes): Bytes
```

`keyring.seal(key)` gives `key` sealed for the calling instance. Each signature
of `keyring` opens `sealed_key` in the host's process and signs as the `crypto`
signature of its algorithm does (§3.2); a sealed key that fails to open rejects
the transaction with the code `host`. A private key that a module stores
reaches state, history, and exports; a sealed key opens in its own instance
alone:

```liasse
signing: {
    public_key: Bytes
    sealed_key: Bytes
}?

mut new_signing_key() {
    let pair = crypto.ed25519_generate();
    .signing = { public_key: pair.public_key, sealed_key: keyring.seal(pair.private_key) };
}
```

The commit of `new_signing_key` holds the public key and the sealed key. Its
history records the calls to `crypto.ed25519_generate` and `keyring.seal` as
`call` entries, and the private key stays in its run.

## 4. Metaproperties

| Metaproperty | Type | Description |
| --- | --- | --- |
| `$history` | `{ created_at: Timestamp, updated_at: Timestamp }` | The instants of the row's insertion and last change to its own stored members. |

For a committed insertion, both timestamps are that commit's instant. A
committed change to the row's own value or stored object members sets
`updated_at` to the modifying commit's instant.
Changes inside a nested stored set and equal-value writes preserve both
timestamps. Migration writes follow these same rules: changing a surviving
row's own stored members updates its timestamp, and inserting a row starts
both timestamps at the migration commit's instant. A rename preserves the
row and its metadata.
Wall-clock adjustments can make `updated_at` earlier than `created_at`.

---

## 5. Template definitions

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
