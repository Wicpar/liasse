# Liasse v0.5 — Consolidated Specification

Status: **standard draft**.

## Abstract

Liasse is a Rust-first application state engine. A package declares typed logical application state together with the operations and external interfaces that observe or change it. This specification defines values, identity, constraints, authorization, atomic admission, ordering, history, and client-visible coherence. A conforming implementation may choose its storage, compilation, caching, partitioning, process placement, and resource allocation strategy while preserving those semantics.

<a id="scope"></a>

## 1. Scope

This document specifies:

- the Liasse package model and authoring syntax;
- typed logical state, identity, references, expressions, views, and mutations;
- external surfaces, authentication, sessions, and live clients;
- modules, temporal buckets, meters, host namespaces, keyrings, blobs, history, migrations, deletion, and erasure;
- runtime admission, ordering, completion, and replay semantics;
- the contract between a Liasse runtime and its Rust host components.

The specification governs observable application behavior. Storage layouts, query plans, caches, materialized values, process topology, and resource scheduling remain implementation choices.

<a id="conformance"></a>

## 2. Conformance and document conventions

### 2.1 Conformance classes

This specification defines four conformance classes:

- A **package** conforms when it satisfies the syntax, typing, identity, dependency, and static validation rules of its declared `$liasse` version.
- A **runtime** conforms when it loads conforming packages and preserves every normative state, execution, history, authorization, and client-coherence rule in this document.
- A **client binding** conforms when its calls, watches, retries, authentication selection, and completion behavior preserve the protocol semantics defined here.
- A **host component** conforms to the typed contract it registers, such as a namespace, key provider, or blob connector.

A runtime claiming support for Liasse v0.5 MUST implement the complete normative language defined by this document. Host components remain explicit dependencies: a package requiring an unavailable or incompatible component fails validation before activation.

### 2.2 Normative language

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **SHOULD NOT**, **RECOMMENDED**, **MAY**, and **OPTIONAL** are to be interpreted as described in BCP 14 when, and only when, they appear in all capitals.

Normative provisions define required observable behavior. Statements about implementation strategies describe permitted realizations only when they use a normative keyword.

### 2.3 Informative material

Section 3 and the worked examples are informative. Examples, notes, rationales, and implementation illustrations are informative unless a normative section explicitly incorporates a rule stated outside the example. The normative annexes are part of the specification.

### 2.4 Core terminology

An **application** is one logical Liasse state together with its active module composition and retained history. A **`.liasse` artifact** is the portable representation of one application or module instance. It contains that instance's definition, resources, selected state, owned history, blobs selected for inclusion, and its direct child-module instance artifacts. A **loaded model** is the validated semantic model instantiated from the active definition; its internal representation belongs to the implementation.

A **package instance** is one installed application or module definition together with its immutable instance incarnation, configuration, boundary bindings, owned state, and direct child instances. A **history point** identifies one exact retained state of one package instance. A **lineage** is one continuation of that history from genesis or from an earlier point. The **active composition** selects the live point of the root instance and of every installed child instance recursively.

The package's `$model` defines logical **state**. A **row** is one structured value in a keyed collection. Its **key** is the application-defined identity used for addressing and references. Its **incarnation** is the immutable continuity of that inserted row from creation until deletion, including any atomic rekey. A **view** is a typed read-only result derived from state. A **mutation** is a typed sequential program proposing one atomic state transition.

A **request** is one external invocation. **Admission** validates and integrates its proposed transition at one final serial position. A successful state or composition change creates a **commit**. A commit is final once admitted, including a package-only transition whose writable state delta is empty.

A **surface** is a named external API entry. An **actor** is the application row resolved by authentication for one external request. A **session** is an application-defined row that may carry continued authentication state. A **client frontier** identifies the latest application commit and temporal observation reflected by one live result.

Feature-specific terms are defined in the chapter that introduces them.

### 2.5 Syntax conventions

Hjson examples show the recommended authoring form. The built `liasse.json` definition is strict normalized JSON inside a `.liasse` artifact. In expressions, `.` denotes the current value or receiver, `@name` denotes a mutation or view parameter, and names beginning with `$` are Liasse declarations or structural bindings. Annex C contains the compact syntax index.

Application declaration names MUST begin with an ASCII letter and contain only ASCII letters, digits, and `_`. Names beginning with `$` are reserved by Liasse. Package names are dot-separated lowercase identifiers containing `a`–`z`, `0`–`9`, and `_`, with each component beginning with a letter. Unknown members in a declaration object are invalid unless that declaration explicitly accepts application-defined member names.

---

<a id="overview"></a>

## 3. Overview

> This section is informative.

Liasse packages describe typed application state, the read-only views derived from it, the atomic mutations allowed to change it, and the external surfaces through which clients use it. The package defines observable behavior. The runtime chooses how to store, compile, cache, partition, and execute that behavior.

### 3.1 Minimal mental model

A Liasse model is a logical tree. Its nodes may contain:

- scalar fields and static structs;
- keyed collections and sets;
- computed values and views;
- mutation declarations;
- public surfaces and authenticated roles;
- module spaces, buckets, meters, blobs, keyrings, and history policy.

The tree defines observable paths, scopes, identity, constraints, and results. An implementation chooses physical tables, documents, indexes, partitions, caches, compiled queries, and node placement.

A keyed collection gives application-defined identity to rows. A view derives read-only data from state. A mutation is one atomic program. A surface exposes named views and mutations. Authentication resolves an application row as `$actor`; a role determines which surfaces that actor may use. Every successful mutation becomes one final commit and advances the calling client's live views through that commit.

### 3.2 A complete small application

The smallest useful package can define data, behavior, a view, and a public API in one model. Seeing the whole path first makes later chapters refinements of a working application rather than isolated language features.

```hjson
{
  "$liasse": 1
  "$app": "example.tasks@1.0.0"

  "$model": {
    "tasks": {
      "$key": "id"

      "id": "uuid = uuid()"
      "title": {
        "$type": "text"
        "$normalize": "string.trim(.)"
        "$check": ["size(.) > 0", "A title is required"]
      }
      "done": "bool = false"
      "created_at": "timestamp = now()"

      "$mut": {
        "complete": [
          ".done = true"
          "return . { id, title, done, created_at }"
        ]
      }
    }

    "$mut": {
      "add_task": [
        "task = .tasks + { title: @title }"
        "return task { id, title, done, created_at }"
      ]
    }

    "open_tasks": {
      "$view": ".tasks[:task | !task.done] { id, title, created_at, $sort: [-created_at] }"
    }

    "$public": {
      "tasks": {
        "$view": ".open_tasks"
        "$mut": {
          "add": ".add_task"
          "complete": ".tasks[@id].complete()"
        }
      }
    }
  }
}
```

The package declares:

- a keyed `tasks` collection;
- generated and defaulted fields using `type = default expression`;
- one normalized and checked field;
- two atomic mutation programs;
- one projected and sorted view;
- one public surface exposing that view and two named calls.

The mutation parameter types are inferred. `@title` inherits the type accepted by `tasks.title`; `@id` inherits `tasks.$key`. A prototype remains available when inference would leave more than one valid type.

### 3.3 Calling and watching

A call executes one named mutation; a watch maintains one named view. Together they form the minimal client model for changing and observing application state.

```text
call public.tasks.add { title: "  Read the specification  " }
  -> { id: <uuid>, title: "Read the specification", done: false, created_at: <timestamp> }
```

A client may subscribe to the surface view:

```text
watch public.tasks
  <- init  [...]
  <- patch [...]
```

When `add` returns successfully:

1. the mutation has obtained its final serial position;
2. every statement has executed;
3. normalization, checks, keys, refs, uniqueness, permissions, meters, and other applicable guarantees have cleared;
4. the commit is final and cannot be revoked;
5. the returned view is evaluated from the committed state;
6. every active live view on the same logical client connection has advanced through that commit.

Another request may already have committed after it. The guarantee is **at least the returned commit**, rather than a promise that its state remains globally latest.

### 3.4 Reading paths

The shortest implementation path is:

1. package structure and state model;
2. expressions, views, and mutations;
3. package loading;
4. external surfaces, authentication, and clients.

Modules, buckets, meters, host namespaces, keyrings, blobs, history, migrations, and erasure are independent feature chapters. Detailed type, ordering, grammar, identity, integrity, and compatibility rules are collected in the normative annexes.

---

## Part I — Package and application model

<a id="package-structure"></a>

## 4. Package structure

A `.liasse` artifact is the common portable form for a root application or a module instance. A freshly built artifact carries its genesis state and contained genesis modules. An exported artifact carries a selected state and the retained history chosen for export. The same archive can therefore be created as a fresh instance, restored with its exported identity, or reconciled with an existing instance.

### 4.1 Artifact and definition shape

A `.liasse` artifact is a ZIP64 archive with this root structure:

```text
mimetype
manifest.json
liasse.json
resources/
state/
history/
blobs/
modules/
```

`liasse.json` is the canonical strict-JSON application or module definition. `resources/` contains resources referenced by that definition. `manifest.json` identifies the represented instance, selected history point, current direct-module composition, included entries, and their checksums. `state/`, `history/`, `blobs/`, and `modules/` are defined by [History, artifacts, and reconciliation](#history).

The authoring form of an application definition is:

```hjson
{
  "$liasse": 1
  "$app": "vendor.application@1.0.0"
  "$semantics": { ... }   // optional
  "$requires": { ... }    // optional host namespaces
  "$resources": { ... }   // optional packaged resources
  "$types": { ... }       // optional reusable shapes
  "$model": { ... }
  "$data": { ... }        // optional genesis data
  "$history": "all"       // optional minimum history policy
  "$migrations": { ... }  // optional exact-source migrations
}
```

A module definition uses `$module` instead of `$app` and MAY also declare `$config`, `$use`, `$deps`, `$expose`, and `$migrations`.

`$liasse` is required and selects the complete language generation used to validate and instantiate the definition. The value `1` selects the language defined by this specification. A runtime MUST reject an unsupported value before interpreting any other declaration. Semantic versions under `$app` or `$module` version the definition itself and are independent of the `$liasse` language generation.

`$resources` maps logical resource names to entries below `resources/`:

```hjson
"$resources": {
  "invoice_template": {
    "$path": "resources/invoice.html"
    "$media": "text/html"
    "$sha256": "..."
  }
}
```

`$path` is a relative archive path, `$media` is its media type, and `$sha256` is the digest of the exact entry bytes. Paths MUST remain inside the archive root and MUST identify one entry. Every declared digest is verified before activation. Package expressions and registered namespaces refer to resources by logical name.

### 4.2 Authoring and definition identity

The human-written source MAY use Hjson. Building converts it to canonical `liasse.json` and collects its resources into the `.liasse` artifact. Comments, optional commas, unquoted keys, and multiline strings are authoring conveniences and carry no meaning after the build.

The **definition identity** comes from canonical `liasse.json`. Its resource descriptors contain exact digests, so the identity covers the active definition and resource contents independently of ZIP entry order, compression method, timestamps, and container metadata. The complete artifact has separate transfer-integrity checksums defined in Annex D.

Loading validates the definition, resources, state, history, interfaces, and dependencies, then instantiates a loaded model. The implementation MAY normalize, compile, index, or discard parsed forms after activation. Runtime forms have no portable identity.

In `$data` and expanded `$default` positions, a string beginning with `=` is an expression. Prefix a leading `'` to store a literal string beginning with `=`.

```hjson
"enabled": "= #company.plan == 'pro'"
"formula": "'= total + tax"
```

A `$data` or expanded `$default` value of `=` alone — the expression marker with an empty body — is a static (load-time) error; the expression after `=` MUST be non-empty. It is neither the literal text `=` nor an empty result. A literal `=` is written `'=` (the leading `'` escape above).

Other expression positions, such as `$view`, `$check`, `$normalize`, and mutation bodies, contain bare expressions.

### 4.3 Application, module, and instance identities

`$app` identifies an application definition; `$module` identifies a reusable module definition. Their values combine a stable name and semantic version:

```hjson
"$app": "vendor.application@1.0.0"
"$module": "vendor.feature@1.0.0"
```

The name identifies the compatibility line. The version controls update compatibility as specified in Annex E. The canonical `liasse.json` identity identifies the exact definition independently of registry or archive location.

Each active application or module installation is a package instance with an immutable incarnation. Two installations of the same definition have separate incarnations, states, histories, configurations, bindings, and descendants. Rekeying or remounting an existing instance preserves its incarnation; uninstall followed by installation creates a new one.

A module instance owns its state and history at the module boundary. Parent and peer modules inspect that state through bound views and change it through mutations explicitly bound by an exposed interface. Definition, configuration, binding, and owned-state changes are represented in that instance's history. Child-owned changes remain in each child's independent history.

Creating from an artifact assigns fresh instance incarnations recursively. Restoring preserves exported incarnations. Reconciliation matches retained histories by incarnation.

### 4.4 Observable semantic choices

`$semantics` selects application-visible choices whose result can vary meaningfully, such as decimal division or timestamp representation. Physical and optimization choices stay with the implementation; only observable behavior belongs in the package.

The common case uses standard defaults. A package MAY select observable choices that affect arithmetic or time representation:

```hjson
"$semantics": {
  "timestamp_precision": "us"
  "decimal_division": {
    "scale": "postgres"
    "rounding": "half_away_from_zero"
  }
}
```

Each member of `$semantics` names one standard semantic choice. A scalar selects one mode; an object groups the settings of that choice. The declared Liasse version defines the accepted choice names and members.

A timestamp field MAY override the package precision:

```hjson
"captured_at": {
  "$type": "timestamp"
  "$precision": "ns"
  "$default": "= now()"
}
```

Supported timestamp precisions are `s`, `ms`, `us`, and `ns`. Every engine MUST preserve the selected observable semantics.

Type forms and canonical wire encodings are collected in Annex A.

---

<a id="state-model"></a>

## 5. State model

The `$model` object defines the logical shape, identity, constraints, and relationships of application state. These declarations determine observable values and valid states while leaving physical representation, indexing, caching, and materialization to the implementation.

### 5.1 Fields and defaults

A field is a writable typed value in a logical row or static struct. A default supplies its insertion-time value when the caller omits it. The shorthand keeps ordinary data concise while expanded forms add normalization and checks where needed.

Primitive field:

```hjson
"name": "text"
```

Optional field:

```hjson
"email": "text?"
```

Field with a default:

```hjson
"enabled": "bool = true"
"created_at": "timestamp = now()"
"id": "uuid = uuid()"
```

`T = expression` declares a writable field of type `T`. The default supplies its initial value when an insertion omits the field. A supplied value, including `none`, takes precedence.

Expanded form:

```hjson
"email": {
  "$type": "text"
  "$optional": true
  "$default": "= ''"
  "$normalize": "string.lower(string.trim(.))"
  "$check": ["size(.) <= 320", "Email is too long"]
}
```

All members of an expanded field declaration refine that one field. A plain nested object declares a static struct.

A default is any value or view expression visible from its declaration scope whose result is assignable to the field type:

```hjson
"account_order": "int = count(/orders[:order | order.account == .account]) + 1"
"tax_rate": "decimal = /tax_rates[{ country: .country, kind: .kind }].rate"
```

Fields and object members form named structure; their source order has no semantic effect. During insertion, `.` denotes the provisional row containing every supplied value and every default whose dependencies have resolved. Defaults and computed insertion values form one dependency graph. The model is valid when that graph is acyclic, and the implementation MAY evaluate it in any topological order.

External reads made by a default observe the transaction state produced by preceding mutation statements. Rows created by one bulk insertion statement become selectable together only after every row in that statement has resolved. A model requiring one inserted row to observe another uses separate mutation statements to establish that sequence. Generative values such as `uuid()` and `now()` are produced once for the admitted insertion.

These two generative values differ, and the difference is observable whenever one insertion creates several rows. `now()` observes a single instant fixed for the whole admitted request, so every call in the request yields the same timestamp (Annex A.5) — a timestamp means the instant the transaction ran. `uuid()` instead yields a fresh, distinct value on every evaluation, including each row when one field-default call site is evaluated across the several rows of a single insertion; two rows of one request therefore never share a generated `uuid()`. "Produced once" is a recording guarantee, not a statement that distinct occurrences share a value: a generated value that enters committed state or another durable admission fact is materialized at admission, reused verbatim on replay and in audit projections, and never re-generated. This is what lets the standard machine-generated surrogate-key idiom (`id: "uuid = uuid()"` with `$key: "id"`) bulk-insert many rows without a self-collision on the key, while a state-derived default (`count(/items) + 1`) still observes the same pre-statement state every row of one bulk insertion sees.

### 5.2 Computed values

A computed value is a read-only value determined by an expression. It exposes derived state without prescribing whether the runtime computes, caches, indexes, or materializes it.

```hjson
"subtotal": "decimal"
"tax": "decimal"
"total": "= .subtotal + .tax"
```

A computed value is read-only and determined by its expression. It participates in views, checks, sorting, and projections like any other value. The engine MAY compute, cache, materialize, index, or incrementally maintain it.

A computed expression yielding `none` produces an absent optional value.

### 5.3 Static structs

A static struct is a named object whose fields share the identity and lifecycle of its containing row. It groups related values without introducing separately keyed identity.

A plain object is a static struct:

```hjson
"address": {
  "line1": "text"
  "line2": "text?"
  "city": "text"
  "country": "text"
}
```

Struct members are unordered named fields. Their dependency relationships determine evaluation where expressions refer to one another. Structs MAY contain fields, structs, sets, views, and nested keyed collections.

An object's node kind is fixed by exactly one kind marker among `$key`, `$set`, `$view`, `$ref`, `$enum`, `$type`, `$keyring`, `$modules`, and `$like` (Annex C.2); `$bucket` composes with `$key` (§14) and otherwise declares a source-backed bucket. A plain object bearing none of these markers is a static struct. An object bearing two mutually-exclusive kind markers — for example both `$key` and `$set` — has no uniquely determined node kind and is a static (load-time) error that names both conflicting markers; no marker silently wins.

### 5.4 Keyed collections

A keyed collection is a set of rows with application-defined identity. Keys make rows addressable, referenceable, incrementally observable, and safely mergeable into views.

`$key` turns a shape into a collection and defines row identity:

```hjson
"companies": {
  "$key": "id"
  "id": "text"
  "name": "text"
}
```

Identity is application-defined. Keys MAY be natural, generated, or composite.

Generated key:

```hjson
"people": {
  "$key": "id"
  "id": "uuid = uuid()"
  "name": "text"
}
```

Composite key:

```hjson
"vat_rates": {
  "$key": ["country", "code"]
  "country": "text"
  "code": "text"
  "rate": "decimal"
}
```

A string `$key` names one key field. An array groups several fields into one composite key, in the listed order.

A collection key is unique within its parent row. Nested collections therefore have row identity plus ancestor identity:

```text
/companies/acme/offices/paris/rooms/main
```

Assigning a new value to any key field performs an atomic **rekey**. The runtime MUST update every ref that targets the row, every ref that targets one of its descendants whose full identity contains that key, and every affected live-view identity in the same transition. The row and its descendants retain their logical incarnation and history continuity; `$on_delete` does not run. A collision, invalid rewritten ref, or resulting constraint failure rejects the complete transition.

A `$key` entry names declared fields. Source-backed bucket collections MAY additionally use their implicit structural bindings as described in [Buckets](#buckets).

### 5.5 Sets

A set stores unique membership values without per-member payload. It is the compact form for tags and pure relations; keyed collections cover relations carrying additional fields.

A set stores unique payload-free membership:

```hjson
"tags": { "$set": "text" }
```

Set of refs:

```hjson
"reviewers": {
  "$set": { "$ref": "/accounts" }
}
```

The value of `$set` is the shape of every member. A member shape is a present value type — `none` is absence, not a value, so it is never a set member, and the member shape of a set is never `optional<T>`. Adding `none` to a set is a no-op that leaves the set unchanged, mirroring set membership below. Initial membership comes from data or mutations. Sets have canonical read order from the element type's total order. Membership is mathematical: repeated input values collapse to one member, adding an existing member leaves the set unchanged, and removing an absent member leaves it unchanged.

When a containing row or struct is created, an omitted child set or keyed collection starts empty. A supplied set initializer is a set value. A supplied child-collection initializer is a typed keyed row view. The complete nested result is validated atomically with the containing insertion. `$data` uses the keyed map form defined in [Seed and import data](#loading).

Use a keyed collection when a relation carries payload:

```hjson
"members": {
  "$key": ["company", "account"]
  "company": { "$ref": "/companies" }
  "account": { "$ref": "/accounts" }
  "role": "role"
}
```

### 5.6 References and delete decisions

A reference is a typed target key that MUST resolve to an existing row. References preserve relational integrity, while deferred `$on_delete` keeps deletion policy absent until deletion actually becomes possible.

A ref exposes the target's key type and MUST resolve to one occurrence in its declared target relation:

```hjson
"owner": { "$ref": "/accounts" }
```

The application-visible value is the target's current typed key. Internally, the ref remains attached to the target row incarnation, so atomic rekeying changes the visible key while preserving the relationship. Deleting and reinserting the same key creates a new incarnation and does not transfer existing refs.

A ref MAY target a keyed view whose identity is inherited from stable source-row incarnations. When the referenced occurrence leaves that target relation, ordinary deletion planning applies regardless of whether the cause is explicit deletion, collection replacement, view-membership change, bucket lifecycle, package update, module disable, interface withdrawal, or uninstall.

`$on_delete` stays implicit while the target's owning module declares no operation that MAY delete the target. Omission means that the decision is deferred; it supplies no cascade, patch, or restriction behavior.

When the owning module introduces a mutation or another lifecycle operation that MAY delete the target, every inbound ref MUST choose its behavior before that declaration can become active:

```text
restrict   preserve the target while this ref exists
cascade    delete the containing row/member with the target
none       clear this optional ref
= patch    apply the declared patch to the containing row
```

`$on_delete` accepts a policy name or an expression beginning with `=`. `none` is valid for an optional ref and clears that field; it is the shorthand for a patch assigning `none` to the referencing field. A patch expression returns one patch for the referencing row, whose object members identify every field changed when the target disappears.

Example clearing an optional ref:

```hjson
"owner": {
  "$ref": "/accounts"
  "$on_delete": "none"
}
```

This expands to:

```hjson
"owner": {
  "$ref": "/accounts"
  "$on_delete": "= { owner: none }"
}
```

A module owns both its private data and the mutations over that data, so the checker can defer this requirement safely while no declared operation can remove the target occurrence. A ref to a relation whose membership can disappear through an active mutation, lifecycle, replacement, or conditional boundary MUST already define its policy. A ref crossing a module boundary MUST declare `$on_delete` immediately because the target package controls its own state and exposed membership. The complete rules are in [Deletion and erasure](#deletion).

### 5.7 Additional uniqueness

`$unique` declares candidate keys in addition to the row’s primary `$key`. It expresses domain identity and collision rules without changing the row address.

`$unique` lists candidate keys. A field name defines one single-field candidate key; an inner array groups the fields of one composite candidate key, in the listed order. The outer array therefore enumerates constraints, while each inner array forms one key.

```hjson
"users": {
  "$key": "id"
  "$unique": ["email", ["country", "tax_id"]]

  "id": "uuid = uuid()"
  "email": "text"
  "country": "text"
  "tax_id": "text"
}
```

Field shorthand:

```hjson
"email": { "$type": "text", "$unique": true }
```

Field `$unique: true` adds one single-field candidate key for that field.

A candidate key containing optional fields enforces uniqueness among rows for which every component is present. A row with `none` in any component does not conflict through that candidate key. Every present component MUST have a key-eligible type. Nested uniqueness is scoped to the parent row.

### 5.8 Named and recursive shapes

A named shape is a reusable structural state or interface definition. It avoids repetition and gives recursive or cross-module contracts a stable readable name.

`$types` maps reusable type names to shapes using the same shape grammar as `$model`:

```hjson
"$types": {
  "role": { "$enum": ["admin", "member"] }

  "company": {
    "$key": "id"
    "id": "text"
    "name": "text"
    "subcompanies": "company"
  }
}

"companies": "company"
```

Type names provide reuse and readable contracts. Satisfaction is structural: a value or view with the required fields, types, and identity satisfies the shape.

Structural satisfaction is closed. A value or view satisfies a struct or named shape only when it presents exactly the declared members: every required member present and type-conformant, every optional member present-and-conformant or absent (carrying `none`), and no member the shape does not declare. A value carrying an undeclared member does not satisfy the shape. This governs produced and returned values; the treatment of undeclared members in an untrusted external argument object is a separate question and is not decided by this rule.

`$like` remains available for positional recursion:

```hjson
"subcompanies": { "$like": "^" }
```

Its value is a lexical shape reference: `^` names the immediately containing shape, `^^` its parent, and so on. The field adopts that shape contract while retaining its own location and data.

### 5.9 Enums

An enum is a closed set of checked labels. It gives application states a finite typed vocabulary and a deterministic declaration order.

```hjson
"status": { "$enum": ["draft", "active", "closed"] }
```

The `$enum` array lists distinct accepted labels and establishes their declaration order. Enum values are checked labels, and their default total order follows that order.

### 5.10 Struct and row checks

`$check` MAY appear on a static struct or keyed collection shape to constrain the complete value rather than one field. In that position, `.` is the prospective struct or row after defaults and normalization have resolved.

```hjson
"period": {
  "starts_at": "timestamp"
  "ends_at": "timestamp"
  "$check": [".ends_at > .starts_at", "The period must advance"]
}
```

A row or struct `$check` accepts the same one-check and multiple-check forms as a field check. Field checks, nested shape checks, and containing shape checks are all state constraints; package logic MUST NOT depend on their diagnostic evaluation order.

---

<a id="expressions"></a>

## 6. Expressions

An expression computes a typed value from logical state and local bindings. Expressions are used by defaults, computed values, selectors, checks, mutations, authentication, and views. Their static type and effect class are checked when the package is loaded.

### 6.1 Expression classes

Liasse scalar expressions use CEL syntax, operator precedence, and static typing. Liasse adds typed row and view selectors, projection blocks, structural bindings, mutation operators, `none`, and host namespaces.

Liasse checks four expression classes:

```text
value       reads and constructs scalar or structured values
view        reads and constructs row streams
mutation    changes state, calls mutations, asserts, binds, or returns
shape       declares a type or data shape
```

Value and view expressions share access syntax. Mutation expressions add state-changing operators.

### 6.2 Roots and bindings

A root chooses the state scope from which an expression begins; a binding gives a value a local name inside that expression. Explicit roots and concise bindings make nested application logic readable without losing scope.

```text
/             package root
.             current value or row
^, ^^         lexical parent scopes
#name         imported module or parent surface
@name         mutation or view parameter
name          local or row binding
$name         structural runtime binding
none          absent optional value
```

Structural bindings exist only in the feature context that defines them. Authentication provides `$credential`, `$proof`, `$session`, `$actor`, and `$auth_name`; module expressions provide `$config`; buckets provide `$created`, `$source`, `$from`, `$until`, and `$index`; migrations provide `$old`; and delete patches provide `$target`.

Examples:

```text
/companies["acme"]
.name
^.plan
#people[@id]
@amount
project.name
$actor.$key
```

### 6.3 Field access and selectors

Field access follows one object member; a selector chooses keyed rows or a filtered set of rows. Selectors are the relational access mechanism used by reads, writes, refs, and joins.

```text
receiver.field
collection[key]
collection[key_a, key_b, key_set]
```

Examples:

```text
.people[@id]
.people[@lead, @manager, .reviewers]
.tax_rates[{ country: @country, code: @code }]
```

Comma-separated selector operands are independent key sources, and their selected rows are concatenated in operand order. A composite-key lookup uses one object operand naming each key component.

Every selector returns a row view. One scalar or composite key contributes zero rows when the key is absent and one row when it exists. A set or row view of keys contributes the existing target row for each input occurrence. Selection preserves operand order and repeated keys; a set contributes keys in the target collection's canonical order.

A context requiring one row, such as a row-mutation receiver, `$actor`, `$session`, or conversion to a scalar row value, MUST receive exactly one occurrence. Zero or several occurrences reject that evaluation. Repeated occurrences remain distinct in ordinary view results and receive deterministic occurrence identities for live patches as defined in Annex D. Mutation writes deduplicate selected target rows by row incarnation before applying one statement, preserving the first occurrence order.

Equality between a row and a ref compares target incarnation. Equality between a row or ref and a key of the same declared target compares the current typed key. Values belonging to different target relations are statically incomparable. These rules make expressions such as `task.owner == $actor` identity comparisons while preserving ordinary typed key comparison where a key is supplied explicitly.

### 6.4 Row bindings

A row binding names each row while a selector or projection evaluates. It makes predicates and nested views explicit when `.` alone would be ambiguous.

```text
collection[:name]
collection[:name | condition]
```

```text
.projects[:project | !project.archived]
.projects[:project].tasks[:task | task.assignee == project.lead]
```

`::` binds each traversed collection to its own field name:

```text
.projects::tasks::comments
```

expands to:

```text
.projects[:projects].tasks[:tasks].comments[:comments]
```

Use explicit aliases when names repeat or a shorter name reads better.

### 6.5 Function surface

Logical operators, arithmetic, selectors, aggregation, `has`, `assert`, `uuid`, and `now` are part of the language. Utility functions live in the small built-in namespaces:

```text
hex        encode and decode bytes
base64     encode and decode bytes
sha        standard hashes
string     Unicode-safe text utilities
convert    checked conversions
time       Unix time, date, duration, and period utilities
```

`string.trim` removes the leading and trailing Unicode scalar values that carry the Unicode `White_Space` property (equivalently, the scalars Rust's `str::trim` removes); `string.lower`, `string.upper`, and `string.casefold` apply the corresponding Unicode default case and casefold operations. Non-ASCII whitespace — including U+00A0 NO-BREAK SPACE — is therefore trimmed, and a value consisting solely of `White_Space` scalars normalizes to the empty string; because `$normalize` runs before a `$check` (§8.8), such a value fails a `size(.) > 0` check.

Packages MAY require additional typed namespaces from the Rust context, as described in [Host namespaces](#host-namespaces). Package loading validates every function name, type, determinism class, and namespace contract.

The compact grammar index is in Annex C.

---

<a id="views"></a>

## 7. Views

A view is a typed, read-only result derived from logical state. Views filter, project, group, sort, and nest rows while preserving stable identity where the declaration provides it. The runtime MAY compute, cache, index, materialize, or incrementally maintain a view while exposing the same result and live-update behavior.

### 7.1 View declarations and projection

A view declaration uses `$view` to select its source and an optional projection block to shape its result. Views serve as the common read abstraction for internal reuse, external APIs, role membership, and meter sources.

Pass-through view:

```hjson
"project_index": { "$view": ".projects" }
```

Projected view:

```hjson
"project_index": {
  "$view": ".projects { id, name }"
}
```

Projection fields:

```text
field                 field: .field
@name                 name: @name
binding.field         field: binding.field
binding:              binding: binding
name: expression      explicit output
nested: { ... }       nested structure
```

Example:

```text
.people {
  first,
  last,
  full: first + " " + last
}
```

`.` remains the source row. Projection members are unordered named outputs. They MAY refer to one another when their dependency graph is acyclic; the implementation evaluates them in any valid dependency order.

### 7.2 View identity

A view inherits identity from its source row chain:

```text
.projects                         -> projects.$key
.projects::tasks                  -> projects.$key + tasks.$key
.modules::templates              -> modules.$key + templates.$key
```

Projection changes visible fields while preserving inherited identity.

A projection MAY declare a synthetic `$key` for grouping or a new identity:

```hjson
"totals": {
  "$view": '''.entries::lines {
    $key: account
    account
    debit: sum(group.debit)
    credit: sum(group.credit)
  }'''
}
```

A scalar projection `$key` names one output field. An array groups several output fields into one composite key, in the listed order.

Rows sharing the synthetic key form one group. `group` is the source-row view for that output row. Every non-key source value MUST be aggregated or derived solely from key values.

This constraint applies to every projection that declares a synthetic `$key`, whether the key collapses several source rows into one group or re-identifies each source row under a new identity. Validity is a property of the declaration, not of the current data, so a plain non-key source value is rejected even when the synthetic key is unique per row and no rows actually share it. To carry a source field that is determined by the key, wrap it in an aggregate over `group` — for example `min(group.f)` — which is well-defined and equals the field when the group is a single row.

### 7.3 Sorting and bounds

A sort defines deterministic row order; bounds select a finite ordered region. Stable order is required for windows, allocation, pagination, and reproducible results.

Collections and views MAY declare `$sort`:

```hjson
"$sort": ["name", "id"]
```

Descending:

```hjson
"$sort": ["-created_at", "id"]
```

Structured form:

```hjson
"$sort": [
  { "$by": "name", "$dir": "asc" }
  { "$by": "created_at", "$dir": "desc" }
]
```

The `$sort` array lists successive comparison keys from highest to lowest priority. A leading `-` reverses one key; the structured form expresses the same choice through `$by` and `$dir`.

Default order is key ascending. Sort expressions compare lexicographically. Occurrence identity is appended as the final tiebreaker, so repeated occurrences of the same row remain totally ordered.

Optional values follow PostgreSQL-style absence placement:

```text
ascending    present values, then none
descending   none, then present values in reverse order
```

JSON has its own fixed internal order described in Annex B. `none` remains distinct from JSON `null`.

Bounds apply after sorting. `$skip` discards the first non-negative number of rows; `$limit` keeps at most the next non-negative number:

```hjson
"$view": ".entries { id, date, $sort: [-date], $skip: 50, $limit: 50 }"
```

### 7.4 View combinators

```text
a | b          union, left order then new right identities
a & b          intersection, left projection and order
a - b          difference, left projection and order
cond ? a : b   conditional view
view ?? other  fallback when the first view is empty
[]             empty view
```

Operands share row shape and identity domain. Projection into a common synthetic key can adapt heterogeneous sources.

The set combinators `|` and `&` share one precedence level. A chain repeating a single combinator (`a | b | c`, `a & b & c`) is left-associative. A chain that mixes `|` and `&` without grouping — `a | b & c` — is a static (load-time) error, not a silently chosen grouping: because `|` and `&` each take their row order, projection, and identity from the left operand, the two readings `(a | b) & c` and `a | (b & c)` can yield different rows and a different shape, so the ambiguous chain MUST be parenthesized. Grouping uses `( )` (the CEL expression syntax of §6.1). Difference (`-`) binds at the arithmetic-subtraction level, tighter than `|` and `&`; `??` and the `? :` conditional occupy their own levels above the combinators.

Examples:

```text
.projects[:p | p.archived] & .projects[:p | p.overdue]
.imports - .people
has(#billing) ? #billing.customers : []
(.overdue | .urgent) & .assigned
```

### 7.5 Aggregates

```text
count(view)             -> int
sum(view.field)         -> field numeric type
avg(view.field)         -> decimal?
min(view.field)         -> field type?
max(view.field)         -> field type?
distinct(view.field)    -> set<field type>
```

Absent inputs are skipped. Empty input yields `0` for `count`, numeric zero for `sum`, and `none` for `avg`, `min`, and `max`. `avg` converts every numeric input exactly to `decimal` and performs decimal division under the package semantics; callers use an explicit rounding function when they need another scale or an integer result.

### 7.6 Reference traversal

A ref value is a target key. Dereference uses the normal selector:

```text
/accounts[.owner]
/accounts[.reviewers]
```

A ref to a keyed view uses that view's identity type.

---

<a id="mutations"></a>

## 8. Mutations and validation

A mutation is a typed sequential program that proposes one atomic state transition and MAY return a view of the committed result. Mutations keep writable behavior inside the package: callers select a declared operation and provide typed values rather than submitting executable queries.

### 8.1 Mutation declarations

`$mut` maps names to mutation programs:

```hjson
"$mut": {
  "rename": ".name = @name"

  "transfer": [
    "assert(.balance >= @amount, 'Insufficient funds')"
    ".balance = .balance - @amount"
    "/accounts[@to].balance = /accounts[@to].balance + @amount"
  ]
}
```

A single expression is a one-statement program. An array is one sequential, atomic program. Later statements observe the prospective effects of earlier statements. A mutation program MUST contain at least one statement; the empty statement array `[]` is a static (load-time) error, not a valid no-op commit.

### 8.2 Declaration context and receiver

A mutation declared on a keyed collection shape is a row mutation. Calling it requires a selected row, and `.` is that row:

```hjson
"tasks": {
  "$key": "id"
  "id": "uuid"
  "done": "bool"

  "$mut": {
    "complete": ".done = true"
  }
}
```

```text
.tasks[@id].complete()
```

A mutation declared on a static struct uses that struct as `.`. A mutation declared at the model root uses the root object. A mutation declared on a view uses the view's lexical declaring scope and MAY target the underlying model explicitly.

Collection creation usually belongs to the containing struct or root:

```hjson
"$mut": {
  "add_task": ".tasks + { title: @title }"
}
```

### 8.3 Inferred parameters

CEL typing infers a parameter from every use of `@name`:

```hjson
"rename": ".name = @name"
"complete": ".tasks[@id].done = true"
```

`@name` inherits `.name`'s type and optionality. `@id` inherits `.tasks.$key`. Inference does not copy the target field's default, normalization, or checks; parameters declare those behaviors explicitly when the external input itself requires them. The assigned target still applies its own normalization and state checks.

An explicit prototype resolves ambiguity or declares a structure that the body cannot uniquely infer. Its object maps parameter names to their type declarations:

```hjson
"set_metadata({ metadata: optional<map<text, json>> })": ".metadata = @metadata"
```

All uses of the same parameter MUST infer one compatible type. The resulting parameter shape is part of the external surface contract.

A mutation with no inferred or explicit parameters is called with `()`:

```text
.tasks[@id].complete()
```

The expanded `.complete({})` form is accepted with identical meaning.

### 8.4 Local bindings

A bare new name on the left side introduces a local binding:

```hjson
"create": [
  "row = .people + { name: @name }"
  "return row { id, name }"
]
```

Local bindings hold values, rows, or views for the remainder of the program. They do not change state by themselves.

An insertion constructing exactly one row returns that inserted row. An insertion from a multi-row view returns the inserted row view in source order. Replacing a collection returns the complete normalized replacement view in its source order. Deletion returns the deleted rows as they existed immediately before removal, in selector order after duplicate target identities are removed by first occurrence.

### 8.5 Mutation operators

```text
collection + view             insert constructed rows
collection = view             replace the complete collection
row_source { patch }          patch selected rows
collection - keys             delete rows by key
-row_source                   delete selected rows
field = value                 set a field
field -                       clear an optional field
set_field + values            add set members
set_field - values            remove set members
mutation()                     call a mutation with no parameters
mutation({ args })             call a mutation with named parameters
assert(condition, message)    require an admission condition
return view_or_value           define the response; final statement only
```

`mutation()` is the compact spelling of `mutation({})`; both forms are exactly equivalent. Set addition is union and set removal is difference: adding an existing member or removing an absent member succeeds without changing that set.

### 8.6 Patches

A patch changes selected fields of an existing row or struct. It provides concise partial updates while the full resulting row still passes every constraint.

```text
.people[@id] {
  @name
  -email
  profile {
    locale = @locale
  }
}
```

Shorthands:

```text
@name          name = @name
source.name    name = source.name
.name          name = .name
-field         clear optional field
```

A patch block is one mutation statement. Each member names one field operation, and a nested block descends into that field. Every right-hand expression reads the prospective row at the start of the patch; separate mutation statements express sequential dependencies.

A patch on a row source applies to every selected row.

### 8.7 Insert, replace, and delete

Insert one row:

```text
.people + { name: @name, email: @email }
```

Insert from a view:

```text
.people + .imports { id, name, email }
```

Replace a collection:

```text
.people = .imports { id, name, email }
```

Delete by key:

```text
.people - @id
.people - @ids
```

Delete selected rows:

```text
-.people[:person | person.disabled]
```

One insertion or replacement statement builds its complete prospective row set before validation. Supplied row values and identities are established together; defaults resolve by their dependency graphs and cannot depend on source occurrence order. Where a feature must allocate among several affected rows, such as several spends admitted by one statement, it uses the source view's declared row order. The source view therefore supplies an explicit `$sort` whenever that priority affects results.

Collection replacement matches existing rows by key. A matching row keeps its incarnation and receives the normalized replacement values; an absent existing key is deleted through ordinary `$on_delete` planning; a new key creates a new incarnation. Rekeying remains an explicit assignment to a key field rather than an inferred match between different keys.

The engine validates the complete resulting collection before admission.

### 8.8 Assertions and checks

An assertion is a transition-specific admission condition; a check is a reusable constraint attached to data or behavior. Both reject the complete mutation before any partial effect becomes visible.

Mutation assertion:

```text
assert(.balance >= @amount, "Insufficient funds")
```

Field check:

```hjson
"name": {
  "$type": "text"
  "$check": ["size(string.trim(.)) > 0", "A name is required"]
}
```

Multiple checks:

```hjson
"$check": [
  ["size(.) > 0", "A value is required"]
  ["size(.) <= 320", "The value is too long"]
]
```

For data checks, a bare expression is one check with a generated diagnostic, `[condition, message]` is one check with its message, and `[[condition, message], ...]` lists several checks. The outer array enumerates checks; each inner two-element array couples one condition with its diagnostic. Every check must pass.

Normalization runs before checks:

```hjson
"email": {
  "$type": "text"
  "$normalize": "string.lower(string.trim(.))"
}
```

A failed assertion or check rejects the complete proposed transition and returns a structured diagnostic containing its path, expression, message, and source span when available.

#### Expression effects

Computed fields, views, `$normalize`, and `$check` use pure functions only. Defaults and mutation programs MAY use generated functions. Authentication `$verify` MAY use verifier functions. Provider-backed generated operations are accepted only in write-time mutation positions.

The checker rejects an effect class used in the wrong position while loading the package.

### 8.9 Zero matches and unchanged calls

A filtered bulk operation selecting no rows succeeds as an expression. When the complete program produces no state change, the call returns `unchanged` and creates no commit. If the program ends in `return`, that response is still evaluated against the unchanged state and delivered with the `unchanged` status; the client frontier does not advance.

A keyed row patch targets one existing row; a missing target rejects the call.

Delete by key is a set operation, not a single-row receiver context: `collection - keys` removes each key that identifies a live row and contributes nothing for a key that is absent, exactly as `$set - values` removes present members and ignores absent ones (§8.5). Deleting a key that names no live row therefore stages no change; if the complete program produces no other change the call returns `unchanged`. A `restrict` inbound reference or an undecided `$on_delete` edge still rejects (§21.1, §22.1). This differs from a keyed patch or field write, whose exactly-one-row receiver (§6.3) rejects an absent target.

### 8.10 Returning committed views

`return` MAY appear only as the final program statement:

```hjson
"rename": [
  ".name = @name"
  "return . { id, name, updated_at }"
]
```

The response is evaluated from the final admitted state: the committed resulting state for `committed`, or the unchanged state for `unchanged`. It MAY be a scalar, row, collection, nested view, or structured value.

A mutation response is ephemeral call output. The runtime evaluates it from the admitted state and delivers it after commit. It is retained only by the caller unless the mutation explicitly writes the same value into application state. Operation deduplication records final status rather than preserving or reconstructing the response.

### 8.11 Calls inside programs

```text
#people.rename({ id: .lead, name: @name })
```

Argument shorthand:

```text
rename({ @id, @name })
```

The argument object maps parameter names to values; member order has no effect. The shorthand expands each `@name` entry to `name: @name`. Internal calls execute inside the same atomic program and preserve the external request's `$actor` and `$session` bindings.

### 8.12 Generated and provider values

`uuid()`, `now()`, and registered generated/provider operations produce their value once for an admitted request. "Once" is a recording guarantee (§5.1): a generated or provider result that enters committed state or another durable admission fact is recorded at admission and reused verbatim on replay and in audit projections, never re-generated. It does not make distinct occurrences share one value. `now()` observes a single request-fixed instant, so every call yields the same timestamp (Annex A.5); `uuid()` yields a fresh, distinct value on every evaluation, so one field-default call site evaluated across the several rows of one insertion produces a distinct value per row and two rows never share a generated `uuid()`. Namespace audit projections MAY record a sanitized typed result defined by that namespace; raw request arguments remain call-local.

Pure computed expressions remain functions of logical inputs and MAY be reevaluated freely.

---

<a id="loading"></a>

## 9. Package loading and bootstrapping

Bootstrapping activates a package together with the initial data required to make it usable. The host `load` operation validates and admits the package, its dependencies, migrations, and seed effects as one atomic transition.

### 9.1 Seed data

`$data` mirrors writable state. Scalar fields, optional fields, static structs, and JSON use their canonical values. An omitted optional field is `none`; JSON `null` remains a present JSON value.

A keyed collection is a map from canonical encoded key text to row data:

```hjson
"$data": {
  "companies": {
    "acme": {
      "name": "Acme SAS"
      "tags": ["customer", "priority"]
      "offices": {
        "paris": { "name": "Paris" }
      }
    }
  }
}
```

The map member supplies the local key. A repeated key field MUST agree with it. Nested keyed collections use the same map form. Sets use JSON arrays of member values and collapse duplicates. Omitted child sets and keyed collections start empty.

Computed values, views, source-backed bucket rows, module spaces, and keyring-managed versions cannot be seeded directly. Their values derive from writable state, installed packages, or provider transitions.

Seed rows pass through the same defaults, normalization, checks, key, ref, uniqueness, bucket, and meter rules as mutation inserts. All seeded row identities and supplied values form one prospective state before defaults resolve. Defaults are then evaluated by dependency; source-object member order and field order have no semantic effect.

Within this single atomic seed load, every seeded row identity and supplied value is visible to every seed default: a default such as `count(/items) + 1` observes all sibling seeded rows. §5.1's rule that rows of one bulk insertion become selectable only after resolution sequences external reads across separate mutation statements; it does not subdivide the genesis seed load.

A `= expr` in `$data` is the literal-or-expression position of §4.2 and Annex C.4: it is evaluated once, at the insertion that seeds the field, and its scalar result is stored as ordinary writable state. It is not a computed value (§5.2) and is never re-evaluated; a stored field seeded from a cross-instance expression freezes at insertion.

### 9.2 Host lifecycle operations

The Rust host controls package lifecycle without entering through an application role:

```text
create(artifact)                 create fresh instance identities from the artifact's selected state
open(store)                      open the active composition recorded by a store
load(target, artifact)           update an existing package instance from the artifact definition
import(artifact, policy)         restore or reconcile an artifact with an existing composition
export(selection)                produce a recursive `.liasse` artifact
```

`create`, `load`, and activation after `import` perform complete validation:

1. open the `.liasse` artifact and parse `manifest.json` and `liasse.json`;
2. verify the artifact entries and resource digests;
3. resolve the Liasse language version;
4. resolve required namespaces, providers, and package dependencies;
5. type-check state, expressions, surfaces, auth, modules, meters, blobs, and keyrings;
6. validate compatibility and migrations where an instance already exists;
7. build the complete prospective recursive composition and owned state;
8. check every boundary contract and state constraint;
9. atomically activate the resulting composition.

`open` validates the recorded active composition and required definitions before making the application available. A diagnostic leaves the prior active composition unchanged.

### 9.3 Bootstrap atomicity

The first successful `create` loads the artifact's definition and selected genesis state into fresh instance incarnations recursively, records genesis history, and activates external surfaces in one host-controlled transition. No client, role, actor, or application permission participates in genesis.

Updating an existing package instance validates its definition, migration, owned state, module interfaces, and complete application composition before activation. A definition-only update creates a commit even when writable state remains unchanged. Import, rollback, branching, and reconciliation are defined in [History, artifacts, and reconciliation](#history).

### 9.4 Load outcomes

A lifecycle operation returns one of:

```text
committed   the validated package state and composition became active
unchanged   the requested definition and data effects already apply
rejected    validation failed and the prior application remains active
```

The host receives structured diagnostics for rejected operations. A successful lifecycle operation is final once its resulting composition becomes active.

---

## Part II — External API and clients

<a id="interfaces"></a>

## 10. External interfaces and roles

A surface is a named external API entry. Public surfaces admit unauthenticated calls; roles combine authenticated membership with the surfaces granted to those members. Clients send names and typed values, while the package remains the sole source of executable reads and writes.

### 10.1 Surface declarations

A surface MAY expose a parameterized view, named mutations, or both:

```hjson
"projects": {
  "$params": { "archived": "bool = false" }
  "$view": ".projects[:p | p.archived == @archived] { id, name }"
  "$mut": {
    "create": ".create_project"
    "rename": ".projects[@project].rename"
  }
}
```

Within a surface, `$params` maps input names to field declarations and defaults, `$view` defines its read result, and `$mut` maps external call names to mutation behavior. These members may coexist.

A surface MUST declare at least one of `$view` or `$mut`. A surface exposing neither — an empty surface, or one carrying only `$params` and/or `$recursive` — is not callable or watchable, and is rejected at load.

A surface `$view` (and a `$recursive` `$where`/`$except` predicate) parameter is not inferred: every `@name` such an expression reads MUST be declared in the surface's `$params`, and an `@name` with no matching `$params` entry is a static error at load. §8.3 parameter inference applies to mutation bodies only, where each `@name` use is an assignment or key selector with a target field to anchor its type; a surface's read positions have no such anchor, and the surface's input shape is its public wire contract, so it is stated explicitly in `$params` rather than derived from an interior expression.

The wire carries the surface name, mutation name, and typed values. It carries no executable expression.

A surface `$mut` is always a map:

- a value naming a declared mutation exposes that mutation;
- a value containing a mutation expression or array defines an inline program for that surface;
- the map key is the external call name.

A declared row-mutation reference MUST select exactly one receiver before naming the mutation. In `.projects[@project].rename`, `@project` selects the row bound as `.` for `rename`; the surface parameters are the selector parameters combined with the referenced mutation's parameters under ordinary type inference. A bare collection such as `.projects.rename` is not a row receiver. Zero or several selected rows reject the request. An explicit call may rename, derive, or fix arguments:

```hjson
"rename": ".projects[@project].rename({ name: string.trim(@new_name) })"
```

A surface view does not implicitly select or authorize mutation receivers. Any receiver restriction belongs in the receiver expression, the referenced mutation, or its checks.

Mutation arrays remain atomic programs in every context.

### 10.2 Public surfaces

A public surface is callable without an authenticated actor. It covers catalog reads, login entry points, callbacks, and other deliberately unauthenticated APIs while retaining all ordinary validation.

`$public` maps external surface names to surface declarations whose operations begin without authentication:

```hjson
"$public": {
  "catalog": {
    "$view": ".products[:p | p.published] { id, name, price }"
  }

  "login": {
    "$mut": {
      "passkey": ".auth.passkey_login"
      "oidc_callback": ".auth.oidc_callback"
    }
  }
}
```

A public operation has no `$actor` or `$session`. Its mutation still passes every type, check, ref, uniqueness, meter, blob, and provider admission condition.

### 10.3 Roles, actors, and scoped APIs

`$roles` maps role names to role declarations. Inside each role, `$auth` selects accepted authenticators, `$members` defines membership, and every other member name declares a surface granted to that role.

A role defines:

- the authenticator or authenticators accepted by its surfaces;
- the actor rows accepted as members;
- the views and mutations available to those actors.

```hjson
"$roles": {
  "member": {
    "$auth": "session"
    "$members": ".accounts[:account | account.enabled]"

    "tasks": {
      "$view": ".tasks[:task | task.owner == $actor] { id, title, done }"
      "$mut": {
        "add": ".add_task_for_actor"
        "complete": ".tasks[@task].complete_for_actor"
      }
    }
  }
}
```

There is no separate actor registry. Authentication resolves one application row as `$actor`; `$members` decides whether that row holds the role at the request's admission position.

`$members` MAY produce actor rows or refs to them. Its row/key type MUST agree with the authenticator's `$actor` result. The actor holds the role when its exact row identity occurs at least once in the resulting view; repeated occurrences grant no additional authority.

Roles MAY be nested on application rows. Their location defines scope. An external request addresses a scoped role by its containing row identity and role name; a client manifest MAY represent that pair as one opaque role handle:

```hjson
"companies": {
  "$key": "id"
  "id": "text"

  "members": {
    "$key": "account"
    "account": { "$ref": "/accounts" }
    "admin": "bool = false"
  }

  "$roles": {
    "admin": {
      "$auth": "session"
      "$members": ".members[:m | m.admin].account"

      "company": {
        "$view": ". { id, name }"
        "$mut": { "rename": ".rename" }
      }
    }
  }
}
```

### 10.4 Definer authority

A surface grants named access to package-defined expressions. The caller supplies typed parameters; the declared view or mutation determines which state it MAY read or change.

An external operation the caller is not authorized to have served — because the named public surface, role, surface, or call does not exist, is not exposed, or names an internal declaration; or because the resolved actor does not hold the targeted role — fails as an authorization denial (the `denied` class). There is no distinct not-found outcome. The denial MUST NOT reveal whether a surface of the named address exists: for a fixed caller and authentication context, the observable denial — its class and any diagnostic code — for a name that does not exist MUST be identical to that for a name that exists but is not granted to that caller. A runtime therefore evaluates role membership before revealing whether a named surface or call exists, so a caller who is not a member of the targeted role cannot enumerate that role's surface catalog. Membership- or existence-specific diagnostics are permitted only toward a caller that has already established authority over the target.

### 10.5 Recursive surface coverage

Recursive coverage applies one declared surface through a checked descendant relation. It avoids duplicating the same role API across application-owned hierarchies while preserving scope and pruning rules.

A scoped role MAY propagate one surface through a recursive child relation:

```hjson
"company": {
  "$view": ". { id, name, plan }"
  "$mut": { "rename": ".rename" }

  "$recursive": {
    "$field": "subcompanies"
    "$through": ".subcompanies"
    "$bind": "child"
    "$where": "child.plan != 'closed'"
    "$except": "child.id == 'hr'"
  }
}
```

`$field`, `$through`, and `$bind` are required. `$where` is optional and defaults to inclusion; `$except` is optional and defaults to no pruning.

At each level:

1. `$through` yields strict descendants of the current row;
2. `$bind` names one candidate;
3. `$where` includes candidates satisfying its predicate;
4. `$except` removes candidates satisfying its predicate and prunes that branch;
5. the same surface projection and mutations apply to included children; recursion descends only into included candidates (one satisfying `$where` and not satisfying `$except`). A candidate excluded by `$where`, or pruned by `$except`, contributes no output slot, and none of its descendants are surfaced or reparented. `$where` is an allow-list (default include) and `$except` a deny-list (default none) that overrides it; both are hereditary.

The output appears under `$field` as a nested keyed view — a keyed tree in which every node's ancestors are all included. The checker verifies descendant shape, acyclicity, identity, and predicate types.

An external request addresses a covered descendant receiver by the role handle — its containing row identity and role name (§10.3) — together with the descendant's key path from that row down through `$field`/`$through`. Admission re-evaluates the recursive relation along the whole path; a path with any step that is not a strict, `$where`-included, non-`$except` descendant is denied. The role-holding row is the empty path, addressed by the role handle alone.

Membership, recursive edges, filters, and exceptions are re-evaluated at admission. A role change therefore affects subsequent requests immediately in serial order.

---

<a id="authentication"></a>

## 11. Authentication and sessions

An authenticator verifies one credential and resolves an application-defined actor, optionally through an application-defined session row. The selected role then authorizes that actor. Accounts, login identities, devices, session rows, and their relationships remain ordinary application state.

### 11.1 Actor and session lifetime

Only an external authenticated operation introduces `$actor` and, when present, `$session`.

Internal mutation calls, module calls, triggers, cascades, meter allocation, view maintenance, and derived computation preserve the external bindings and create no new actor.

Public operations and engine maintenance execute with no actor. A scheduler, worker, connector, or automation that SHOULD possess application authority enters through an authenticated external role with its own application-defined identity.

### 11.2 Application-defined accounts, logins, and sessions

An **account** is an application row that MAY become `$actor`. A **login** identifies one verified external identity, such as an OIDC issuer/subject pair or a passkey credential. A **session** is an application row representing continued authenticated access for one selected account, login, device, and lifetime.

Keeping these as ordinary model data lets one account have many login methods and sessions, one login map to several accounts, and the application choose its own expiry, revocation, device, and account-selection rules.

```hjson
"accounts": {
  "$key": "id"
  "id": "uuid = uuid()"
  "name": "text"
  "enabled": "bool = true"
}

"logins": {
  "$key": ["kind", "issuer", "subject"]
  "kind": "text"
  "issuer": "text"
  "subject": "text"
  "data": "json"
}

"account_logins": {
  "$key": ["account", "login"]
  "account": { "$ref": "/accounts" }
  "login": { "$ref": "/logins" }
}

"sessions": {
  "$key": "id"
  "$bucket": ".expires_at"

  "id": "uuid = uuid()"
  "account": { "$ref": "/accounts" }
  "login": { "$ref": "/logins" }
  "device": "text?"
  "expires_at": "timestamp"
  "revoked": "bool = false"
}
```

This supports many-to-many login/account mappings and any number of simultaneous sessions per account, login, or device. The application chooses the account before opening a session and MAY select it automatically or return an account picker to the client.

### 11.3 Authenticator declarations

A package or module scope MAY contain one `$auth` object. Its members are named authenticators or explicit parent aliases:

```hjson
"$auth": {
  "session": {
    "$credential": "bytes"
    "$verify": "cose.verify(/session_keys, $credential)"
    "$session": "/sessions[$proof.session]"
    "$actor": "/accounts[$session.account]"
    "$check": [
      "$proof.auth == $auth_name"
      "!$session.revoked"
    ]
  }

  "api_key": {
    "$credential": "text"
    "$verify": "api_keys.verify($credential)"
    "$actor": "/integrations[$proof.integration]"
    "$check": "$proof.auth == $auth_name"
  }
}
```

Bindings:

```text
$credential   credential supplied for this call
$proof        typed result of $verify
$session      row selected by the optional $session expression
$actor        row selected by the required $actor expression
$auth_name    authenticator explicitly selected by the request
```

External request arguments, including `$credential`, exist for the call lifetime. Application state contains values written explicitly by the mutation. Ordinary diagnostics and audit records contain parameter names, declared types, stable error codes, and namespace-defined sanitized audit projections. `$verify` MAY use a registered verifier namespace and performs no application-state mutation.

When declared, `$session` MUST resolve exactly one row. `$actor` MUST resolve exactly one row. Zero or several rows reject authentication. `$check` runs after those bindings resolve; a scalar is one condition and a sequence lists conditions that must all pass. Their sequence affects diagnostic reporting only.

Authenticator selection is explicit at every external authenticated request. Each role names every authenticator it accepts, and each request names one of them.

### 11.4 Selecting an authenticator

The targeted role determines the accepted authenticators before the credential is inspected:

```hjson
"$roles": {
  "member": {
    "$auth": "session"
    // ...
  }

  "automation": {
    "$auth": ["session", "api_key"]
    // ...
  }
}
```

A string `$auth` accepts one named authenticator. A sequence accepts any listed authenticator. Every authenticated request names both the role and one accepted authenticator, including roles that accept exactly one. Public surfaces use their public address and carry no authenticator selection. A request to a public address that nonetheless carries an authenticator selection or credential is malformed and is rejected — the runtime does not drop the selection and serve the request actor-less, and it refuses the request before verifying the attached credential.

```hjson
{
  "role": "automation"
  "surface": "jobs"
  "mutation": "submit"
  "auth": "api_key"
  "credential": "..."
  "params": { ... }
}
```

The runtime invokes the selected authenticator directly. The verified proof MUST bind to that authenticator through a protected type, audience, issuer, signed claim, or equivalent verifier contract. A key identifier such as `kid` selects a key version inside that authenticator.

### 11.5 Opening a session

A session-opening mutation is an ordinary public mutation. It verifies an external proof through a registered namespace, maps that proof to an application login and account, creates a session row, constructs a token, and returns the result.

```hjson
"passkey_login": [
  "identity = webauthn.verify(@response)"
  "login = /logins[{ kind: 'passkey', issuer: identity.rp, subject: identity.credential }]"
  "mapping = /account_logins[{ account: @account, login: login.$key }]"

  '''session = /sessions + {
    account: mapping.account,
    login: mapping.login,
    device: @device,
    expires_at: now() + time.duration('P30D')
  }'''

  '''token = cose.sign(/session_keys, {
    auth: 'session',
    session: session.$key,
    issued_at: now(),
    expires_at: session.$until
  })'''

  "return { auth: 'session', token, expires_at: session.$until }"
]
```

`webauthn` and `cose` are typed host namespaces required by the package. The package owns the session state and token claims. Keyring operations expose controlled signing while private key material remains with its provider.

The mutation commits before the response is emitted. A received successful response therefore carries an immediately usable token. A connection failure around commit has the outcome rules defined in [Completion, deduplication, and status](#clients). Password verification, when an application chooses it, follows the same external verifier-namespace pattern.

### 11.6 SSO and account selection

An SSO verifier resolves a provider identity; the application maps that identity to one or more local accounts. The mapping stays application-defined while the host namespace owns protocol and cryptographic verification.

```hjson
"oidc_callback": [
  "identity = oidc.verify(@response)"
  "login = /logins[{ kind: 'oidc', issuer: identity.issuer, subject: identity.subject }]"
  "mapping = /account_logins[:m | m.login == login]"
  "assert(count(mapping) > 0, 'No account is linked')"
  "return mapping { account: .account, name: /accounts[.account].name }"
]
```

A follow-up public mutation chooses one returned account, creates the session, and signs the token. The application defines provider acceptance, claim mapping, provisioning, account selection, session duration, and device policy. The OIDC namespace defines protocol and cryptographic verification behavior.

The same pattern supports passkeys, signed challenges, API keys, and other verifier namespaces.

### 11.7 Revocation and expiry

The session collection's `$bucket` makes only active intervals visible through `/sessions`. The authenticator also checks application revocation state.

```hjson
"revoke": [
  ".revoked = true"
  "return . { id, revoked }"
]
```

A later request using that session fails authentication or role admission. A request already committed remains final.

### 11.8 Multiple simultaneous sessions

A client MAY hold several credentials at once:

```text
application + authenticator + local session label
```

Examples:

```text
session / personal account
session / work account
billing-session / customer 42
api-key / automation
```

Each call selects a compatible credential for the targeted role. Liasse defines no global current session.

Each live subscription is attached to one authentication context. A single network connection MAY multiplex subscriptions from several sessions. The connection-level completion barrier advances every subscription through a successful call's commit, while each subscription continues to apply its own authorization and projection.

### 11.9 Authentication across module scopes

Each module scope MAY declare its own `$auth`.

- A child surface exposed directly to clients uses the child's authenticator selection.
- A parent role wrapping a child mutation authenticates at the parent surface. The child call is internal and receives the parent's `$actor` and `$session`.
- A child MAY alias an authenticator explicitly exposed by its parent module space when both APIs SHOULD share one session system.
- Installing a module alone creates no external endpoint; the host explicitly exposes a surface.

Authenticator names resolve in the scope of the targeted external role. Qualification handles shared or colliding names.

---

<a id="clients"></a>

## 12. Clients and live views

A client invokes named surfaces and MAY keep their views synchronized over a logical connection. A frontier records the exact commit progress of a live result, and the completion barrier tells the caller when a mutation is final and reflected by that connection.

### 12.1 Client manifest and operations

After authentication, the client receives the surfaces granted by the explicitly selected role and actor, including parameter and response shapes. Public surfaces use their public addresses.

The minimal external operations are:

```text
manifest    list available surfaces for a role and authentication context
view        subscribe to a named surface view with typed parameters
call        invoke a named surface mutation with typed parameters
fetch       obtain a permitted blob fetch plan
operation   query a submitted call by operation id
```

The request pipeline is:

1. resolve the targeted public surface or scoped role surface;
2. select and verify the explicitly named authenticator when required;
3. parse parameters against the inferred or explicit shape;
4. apply parameter-declared normalization and checks;
5. stream and verify blob parameters;
6. evaluate current session validity, role membership, receiver selection, and admission conditions;
7. commit atomically or return `unchanged`/`rejected`;
8. advance authorized live results on the calling connection before returning success.

An argument object presented to a `call` or `view` request is closed: it MUST contain only names that are declared parameters of the targeted mutation or view. A member whose name is not a declared parameter — including any reserved `$`-prefixed name — makes the request malformed; the runtime rejects it during parameter parsing (step 3), before admission, with no partial effect. There is no width subtyping over external argument objects, and an undeclared member is never silently dropped.

External parameter values remain call-local unless mutation statements write derived or supplied values into state. Typed values are bound into package expressions; the wire carries no executable source.

### 12.2 Live frontiers and resumable views

A live subscription begins with a complete result and a frontier, then receives ordered patches:

```text
init(frontier, rows)
patch(frontier, operations)
close(frontier, reason)
```

A frontier is an opaque, totally ordered position within one subscription stream. It covers committed application changes and temporal bucket observations. Frontiers from different subscriptions are independent. Resuming from a retained frontier yields the later authorized patches in that stream or a fresh `init` when the runtime has released the required range.

The runtime re-evaluates authentication, session validity, scoped role membership, surface availability, and output projection at every outgoing frontier. When the current state removes that subscription's authority or surface, the runtime emits `close`; the client releases the cached result for that subscription.

Each result occurrence has an opaque occurrence identity. Repeated selections of the same row therefore remain independently patchable. Patch operations are applied in listed order:

```text
insert { $at, $id, $value }
remove { $id }
move   { $id, $to }
update { $id, $value }
rekey  { $id, $key }
```

`$at` and `$to` are zero-based positions in the current result. `update` replaces the occurrence value while preserving identity. `rekey` changes the exposed row key while preserving occurrence and row incarnation. After applying every operation, the client result MUST equal the authorized declared view at the new frontier. A frontier-only patch has an empty operation sequence.

Bounded windows keep large views incremental:

```text
{ $size: n }
{ $size: n, $anchor: row_or_occurrence_identity }
{ $size: n, $anchor: row_or_occurrence_identity, $slide: true }
{ $size: n, $anchor: $first | $last }
```

`$size` is the requested non-negative row count. With no anchor, or with `$first`, the window is the first `n` rows after the surface's own `$skip` and `$limit`. `$last` selects the last `n`. A concrete anchor normally becomes the first row; `$slide: true` centers it as far as the view bounds allow.

The anchor MUST identify exactly one current occurrence when the window opens. If it later leaves the view, the subscription retains its last complete sort tuple plus occurrence identity as an immutable ordered gap. That coordinate determines the window until the occurrence reappears or the subscription is reopened. A surface `$limit` caps the maximum client window.

### 12.3 Completion, deduplication, and status

A mutation commits before its response is emitted. Receiving `committed` proves that the transition is final and that authorized live results on the same logical connection have advanced through that commit. Receiving `unchanged` proves evaluation at the returned frontier.

A connection loss before admission MAY cancel the request. A connection loss while admission or response delivery is in progress leaves the outcome unknown to that client: the request may have committed, and a committed transition remains final.

An external call MAY carry a high-entropy operation identifier. Its scope is the application, public or scoped-role target, selected authenticator when present, and identifier. Reusing the same scoped identifier with an equivalent request provides at-most-once execution. Reusing it with different request metadata rejects the call. A call without an identifier is a new operation on every submission.

Because argument objects are closed (§12.1), a request's identity for this deduplication is its fully-decoded set of declared arguments: two submissions of one operation identifier are equivalent exactly when those decoded argument sets are equal. No ignored-but-present member can silently vary between two submissions the runtime would otherwise treat as one operation.

Operation status is runtime metadata rather than application state or exported history. A retained record reports one of:

```text
pending
committed { frontier, commit }
unchanged { frontier }
rejected { diagnostics }
unknown
```

The runtime MAY expire operation records according to host policy, after which status is `unknown`. For a public call, the high-entropy operation identifier also acts as the capability required to query that record. Mutation responses remain ephemeral and are delivered once for each successfully completed transport exchange; status lookup never reconstructs them.

Before returning `committed`, the runtime advances every still-authorized active subscription on the same logical client connection through the commit. Unaffected views MAY receive only a frontier advancement.

---

## Part III — Composition and advanced features

<a id="modules"></a>

## 13. Modules

A module is a versioned package that owns private state and the mutations over that state. A module space is an application location where independently configured module instances MAY be installed. Explicit bindings preserve ownership, authority, identity, and upgrade boundaries while allowing reusable behavior.

### 13.1 Module packages

```hjson
{
  "$liasse": 1
  "$module": "acme.sales_templates@1.0.0"

  "$requires": { ... }
  "$types": { ... }
  "$config": { ... }
  "$model": { ... }
  "$data": { ... }
  "$use": { ... }
  "$deps": { ... }
  "$expose": { ... }
  "$migrations": { ... }
}
```

Each installed instance owns its private model, data, history, configuration, and dependency bindings. `$config` declares an immutable typed struct for installation values; defaults use the ordinary field rules, and module expressions read it through `$config`. Reconfiguration is an explicit module update and passes compatibility and migration checks.

### 13.2 Module spaces

`$modules` creates an installation space at its exact location:

```hjson
"companies": {
  "$key": "id"
  "id": "text"

  "modules": {
    "$modules": {}
  }
}
```

This creates independent spaces such as:

```text
/companies/acme/modules
/companies/globex/modules
```

Installing the same package in each space creates two independent instances.

### 13.3 Installation and instance identity

`modules.install` creates one named instance inside an existing module space. The request supplies the instance name, exact `.liasse` artifact or compatible package requirement, configuration, optional initial data, and any explicit dependency bindings:

```hjson
{
  "$name": "sales"
  "$module": "acme.sales_templates@1.2.0"
  "$config": { "currency": "EUR" }
  "$data": { ... }
  "$use": {
    "people": "/companies/acme/modules/people"
  }
}
```

The instance name is a non-empty text value, is unique within its module space, and forms the local component of instance identity. Its complete identity is the containing row identity, module-space declaration path, and instance name. Renaming an instance is a rekey and updates refs and bindings under the ordinary key-mutation rule.

Package `$data` is applied first. Installation `$data` then overlays writable scalar and struct fields, merges keyed child collections by key, and unions sets; every resulting value passes ordinary insertion and load validation. An omitted installation `$config` or `$data` uses package defaults and seed data only.

When peer resolution finds several compatible candidates, the install request MUST bind that handle explicitly under `$use`; the binding names one concrete sibling instance and exposed interface. The admitted instance records the exact package and every resolved choice:

```hjson
{
  "$name": "sales"
  "$module": "acme.sales_templates@1.2.0"
  "$source": "sha256:<canonical package hash>"
  "$config": { "currency": "EUR" }
  "$resolved": {
    "company": "$parent.company"
    "people": "/companies/acme/modules/people#people"
  }
  "$absent": ["billing"]
  "$namespaces": {
    "cbor": "liasse.cbor@1#<interface-hash>"
  }
  "$migrations": [ ... ]
}
```

`$resolved` maps import handles to concrete bindings. `$absent` lists unresolved optional handles. `$namespaces` pins local namespace contracts. `$migrations` lists applied migrations in order. Loading validates the package hash, configuration, data overlay, bindings, namespace contracts, interfaces, migrations, and seed effects before the instance becomes active.

Disabling an instance removes its direct surfaces, exports, and peer availability while retaining its private state and history. Enabling revalidates and restores them. Uninstall removes the instance through the ordinary cross-module deletion plan.

### 13.4 Parent-provided surfaces

A module space MAY expose a projected parent capability to its children:

```hjson
"modules": {
  "$modules": {
    "$expose": {
      "company": {
        "$view": ". { id, name, plan }"
        "$mut": {
          "rename": ".rename"
          "set_plan": ".set_plan"
        }
      }
    }
  }
}
```

The surface is row-local. Under Acme's module space it refers to Acme; under Globex it refers to Globex.

The module-space `$expose` object maps child-visible handle names to parent-defined surfaces.

A child imports it explicitly:

```hjson
"$use": {
  "company": "$parent"
}
```

and reads or calls it through `#company`.

Renaming the handle:

```hjson
"$use": {
  "org": "$parent.company"
}
```

### 13.5 Peer dependencies

`$use` maps local handles to bindings supplied by the parent or by sibling module instances. Ordinary members are required; the `$optional` object groups handles whose absence is valid.

A peer dependency binds to a sibling instance in the same module space:

```hjson
"$use": {
  "people": "acme.people/people@1"
}
```

In a peer specification, the part before `/` names the package line, the part after `/` names the exposed interface, and `@1` selects a compatible major version.

The module uses `#people`. Usage sites define the structural contract:

```text
#people.members { id, name }
```

Resolution considers compatible siblings in exactly the same module space:

```text
one candidate      bind automatically
several candidates require an explicit operator binding
zero candidates    reject a required binding
```

Optional peers live under `$optional`:

```hjson
"$use": {
  "$optional": {
    "billing": "acme.billing/customers@1"
  }
}
```

`has(#billing)` tests presence in an ordinary runtime expression. It MAY choose a value or view result, but it never adds or removes declarations; structural presence is controlled only by `$if_module`. Private module data remains in place while an optional binding is absent.

Peer lookup stays within the sibling space. Parent capabilities use `$parent`; private required dependencies use `$deps`.

### 13.6 Private required dependencies

```hjson
"$deps": {
  "tax": "acme.tax@2"
}
```

`$deps` maps local handles to private package requirements; each value names the package line and compatible major.

A `$deps` entry creates a private nested instance owned by the consumer. Siblings cannot address it. Two consumers MAY use different major versions independently.

The consumer couples to the dependency's structural interfaces. Compatible minor and patch updates MAY flow automatically; a major change follows the consumer's own update and migration plan.

### 13.7 Module-presence declarations

`$if_module` makes one declaration conditional on the presence of an optional imported module binding:

```hjson
"billing_summary": {
  "$if_module": "billing"
  "$view": "#billing.summary"
}
```

The string names one handle declared under `$use.$optional`. The guarded declaration is active when that handle is bound to an enabled compatible module instance and absent otherwise. This condition is resolved only when the module is installed, rebound, enabled, disabled, or updated; application data cannot change schema shape.

State owned solely by an inactive guarded declaration remains preserved with the module instance and becomes active again only after the same declaration is revalidated against a present compatible binding. Ordinary expressions MAY still use `has(#billing)` to choose runtime values; they do not add or remove declarations.

### 13.8 Module-space interfaces

A module space declares complete boundary contracts. `$interfaces` maps interface names to a view shape and optional callable mutation contracts:

```hjson
"modules": {
  "$modules": {
    "$interfaces": {
      "templates": {
        "$view": {
          "$key": "id"
          "id": "text"
          "label": "text"
          "lines": "json"
        }
        "$mut": {
          "create({ label: text, lines: json })": {
            "$return": { "$ref": ".templates" }
          }
          "disable({ template: text })": {
            "$return": "bool"
          }
        }
      }
    }
  }
}
```

The interface describes what a parent or peer may observe and call. A mutation contract name contains its explicit parameter prototype. Its object contains `$return`, whose value is the required scalar, struct, ref, row, or view response shape; omission of `$return` declares a response-free mutation. A module binds that contract to private views and mutations:

```hjson
"$expose": {
  "templates": {
    "$view": ".templates[:t | t.enabled] { id, label, lines }"
    "$mut": {
      "create": ".create_template"
      "disable": ".templates[@template].disable()"
    }
  }
}
```

View satisfaction is structural. Mutation bindings MUST satisfy their declared parameter and response contracts. The boundary grants access only to those bound members; private child paths and private mutations remain within the child package.

A view may carry bound mutation names at the same interface boundary. Selecting one exposed row binds its row-scoped mutation receiver through that interface, following the ordinary receiver rule.

### 13.9 Aggregating module data

The parent reads every instance exposing an interface:

```hjson
"available_templates": {
  "$view": '''.modules::templates {
    module: modules.$key,
    template: templates.$key,
    id,
    label,
    lines,
    $sort: [label, module, template]
  }'''
}
```

Inherited identity is:

```text
module instance identity + exposed row identity
```

The parent MAY select one instance or a configured subset using ordinary selectors.

<a id="1310-stateful-services-across-module-boundaries"></a>

### 13.10 Stateful services across module boundaries

Stateful behavior crosses a module boundary through the same explicit view and mutation contracts as ordinary state. Buckets and blob descriptors MAY be projected as ordinary interface values when another package needs to observe them.

A meter belongs entirely to the package instance that declares it. Its limits, sources, spends, allocations, and balance accessors resolve only inside that instance. Another package causes a metered transition by calling a mutation explicitly bound by the owning module:

```hjson
"$interfaces": {
  "credits": {
    "$mut": {
      "consume({ account: uuid, amount: decimal })": {
        "$return": {
          "amount": "decimal"
          "funding": "json"
        }
      }
    }
  }
}
```

The consumer invokes the bound operation through its imported interface:

```hjson
"allocation = #credits.consume({ account: @account, amount: @amount })"
```

The owner performs the meter allocation against its private state. The caller's state changes and the owner mutation participate in the same atomic cross-module transition. Installation location grants only the views and mutations declared by the boundary contract.

### 13.11 Authentication scopes

Each module MAY declare its own `$auth`, `$public`, and `$roles`.

Two forms of external use have distinct behavior:

#### Parent wrapper

A parent surface calls a mutation bound by the child interface. The parent surface authenticates the request; the child receives the established `$actor` and `$session`.

```hjson
"$mut": {
  "create_invoice": "#billing.invoices.create"
}
```

The handle and interface name resolve through `$use` or the containing module-space binding. Direct calls to a child's private model path are invalid.

#### Direct module surface

The host mounts a child public or role surface directly through the runtime surface registry. That surface uses the child's authenticator scope. Installing the module does not mount it automatically.

A parent module space MAY expose selected authenticators to children. Its `$auth` object maps each child-visible authenticator name to one authenticator in the parent scope:

```hjson
"modules": {
  "$modules": {
    "$auth": {
      "host_session": "session"
    }
  }
}
```

A child aliases the exposed authenticator inside its single `$auth` object and selects it explicitly from each role that uses it:

```hjson
"$auth": {
  "session": "$parent.host_session"
}

"$roles": {
  "member": {
    "$auth": "session"
    // ...
  }
}
```

The imported authenticator executes in the parent scope and returns the same `$session` and `$actor` bindings. The child role still applies its own `$members` and surface grants. Unexposed parent authenticators are unavailable to the child. Otherwise clients MAY hold separate simultaneous sessions for the parent and child APIs.

Module execution itself creates no actor. It preserves the actor of an external request or runs as actorless engine maintenance.

### 13.12 Deletion across module boundaries

A module owns its private state and the complete mutation set that can directly change it. Internal refs MAY therefore omit `$on_delete` until the module declares a possible deletion.

A module boundary is a deletion boundary. Every installed submodule instance is assumed removable by its owner, and data owned outside the current module MAY evolve independently. A ref crossing that boundary in either direction MUST therefore declare `$on_delete` at the ref site.

Uninstall applies the same declared ref policies as ordinary deletion. An update applies them only to rows, exports, or instance identities that the update removes. Disabling preserves the instance's private stored state while removing its active boundary occurrences, external surfaces, peer availability, and `$if_module` declarations. Every ref whose declared target occurrence disappears enters ordinary `$on_delete` planning. Uninstall additionally deletes the instance incarnation and owned subtree. Each operation is admitted atomically after the full deletion ripple, bindings, interfaces, and constraints are revalidated.

### 13.13 Seed merge

On first installation, `$data` applies as ordinary inserts.

On update, changed seed data uses a three-way merge among the old package seed, the new package seed, and the current instance state.

For each seeded scalar or struct field, the new seed replaces the value only when the current value still equals the old seed value; otherwise the current value is retained. Keyed child collections merge by key and apply the same rule recursively. A row newly present in the new seed is inserted; a row removed from the new seed is deleted only when its current subtree still equals the old seeded subtree, otherwise it is retained as local data. Sets add members newly present in the new seed and remove old seeded members only when application state still reflects the old seed membership.

Every inserted, changed, or removed value passes ordinary defaults, refs, uniqueness, delete planning, checks, and migrations. The update report lists added, updated, removed, and locally retained paths.

### 13.14 Updates and compatibility

Updating one instance affects that instance only. The engine rechecks:

- the instance's model and migrations;
- parent and peer usage sites;
- private dependency interfaces;
- module-space exposures;
- external surfaces and auth contracts;
- meter, blob, and namespace contracts.

A failing recheck blocks the update before admission.

Within one package major, minor and patch releases MUST preserve or widen the exposed compatibility surface. Both registry publication and package loading reject a narrowing release. Breaking changes use a new major and MAY run side by side.

The compatibility algorithm is specified in Annex E.

### 13.15 Update result

A successful update reports its observable plan and committed result:

```hjson
{
  "$instance": "/companies/acme/modules/sales"
  "$from": "1.1.0"
  "$to": "1.2.0"
  "$migrated": [ ... ]
  "$seeded": [ ... ]
  "$exposed": { "$unchanged": ["templates"], "$changed": [], "$removed": [] }
  "$imports": { "$rebound": [], "$broken": [] }
  "$commit": "<commit id>"
}
```

`$migrated` and `$seeded` list per-item reports in canonical path order. `$exposed` and `$imports` group affected names by outcome, with each name array in canonical text order.

A rejected update returns the same planning context plus diagnostics and no commit.

---

<a id="buckets"></a>

## 14. Buckets

A bucket associates a row, or a row derived from another source, with a logical half-open interval `[from, until)`. An omitted upper bound evaluates to `none` and leaves the interval unbounded. Buckets express lifetimes, reservations, billing periods, and recurring availability without selecting a timer, partitioning, or materialization strategy.

### 14.1 Simple lifecycle buckets

```hjson
"sessions": {
  "$key": "id"
  "$bucket": ".expires_at"

  "id": "uuid = uuid()"
  "account": { "$ref": "/accounts" }
  "expires_at": "timestamp"
}
```

The short form means:

```hjson
"$bucket": {
  "$from": "$created"
  "$until": ".expires_at"
}
```

Every bucket interval is half-open:

```text
[$from, $until)
```

`$created` is the row's recorded admission time. At the upper bound, the row is no longer active.

For a bucketed collection:

```text
.sessions                  rows active at the evaluation time
.sessions.$at(time)        rows active at a specified timestamp
.sessions.$between(a, b)   rows whose intervals intersect the non-empty range [a, b)
.sessions.$all             all extant rows independently of current activity
```

`.$between(a, b)` requires `b > a`; an empty or reversed query range rejects evaluation. Live views receive a temporal patch when a row enters or leaves its active interval. The engine MAY schedule, compute, cache, index, or materialize those transitions.

### 14.2 Explicit lifecycle

```hjson
"reservations": {
  "$key": "id"

  "$bucket": {
    "$from": ".starts_at"
    "$until": ".ends_at"
  }

  "id": "uuid = uuid()"
  "room": { "$ref": "/rooms" }
  "starts_at": "timestamp"
  "ends_at": "timestamp"
}
```

An omitted `$from` uses `$created`. An omitted `$until` evaluates to `none` and represents an unbounded upper interval:

```hjson
"licenses": {
  "$key": "id"
  "$bucket": { "$from": ".starts_at" }
  "id": "text"
  "starts_at": "timestamp"
}
```

`null` remains a JSON value. Bucket bounds use `timestamp?`, whose absent value is `none`. A finite interval MUST satisfy `$until > $from`; a transition producing an invalid interval rejects. Bucket expiration changes active views but does not delete the row. `.$all` continues to expose it until an explicit deletion.

Refs to a bucketed collection resolve against its extant `.$all` identity domain, so an inactive row remains a valid target. Ordinary selectors on the collection still return only rows active at their evaluation time.

### 14.3 Ordering unbounded buckets

The ordinary total order places `none` after present values in ascending order and before them in descending order.

```hjson
"$order": ["$until"]
```

means:

```text
earliest finite end ... latest finite end, unbounded
```

```hjson
"$order": ["-$until"]
```

means:

```text
unbounded, latest finite end ... earliest finite end
```

This rule belongs to optional values generally; meters add no bucket-specific ordering exception.

### 14.4 Source-backed buckets

A source-backed bucket collection derives interval rows from another view and exposes the source identity and interval bounds as structural bindings. This keeps source identity and period boundaries available without redeclaring them as application fields:


```hjson
"access_periods": {
  "$bucket": {
    "$source": ".subscriptions"
    "$from": "$source.starts_at"
    "$until": "$source.ends_at"
  }

  "plan": "= $source.plan"
}
```

Each source row evaluates `$from`, `$until`, and `$repeat` independently. One source row produces one interval row when no repetition is requested. The derived row exposes structural bindings:

```text
$source     source row
$from       interval start
$until      interval end or none
$index      zero for a non-repeating source
```

They remain available in filters, projections, keys, meters, and computed fields without being redeclared as application fields.

Source-backed bucket rows are read-only. Mutations change their source rows or the tables they reference.

### 14.5 Recurring source-backed buckets

Add `$repeat` to generate consecutive periods:

```hjson
"credit_periods": {
  "$bucket": {
    "$source": ".subscriptions"
    "$from": "$source.starts_at"
    "$until": "$source.ends_at"
    "$repeat": "/plans[$source.plan].period"
  }

  "credits": "= /plans[$source.plan].credits"
}
```

For source row `s`, let `b0` be `$from`. Each next boundary is obtained by adding `$repeat` according to the `period` value. Generated row `i` has:

```text
$index = i
$from  = bi
$until = min(bi+1, source-series-until) when the series has an upper bound
```

A clipped final interval is included when its start is below its end. An omitted series `$until` generates periods indefinitely. Each generated period still has a finite `$until` supplied by its next boundary. Every recurrence step MUST advance strictly from the prior boundary; a zero, negative, or otherwise non-advancing period rejects the source row or the transition that produced it. A finite series bound MUST be greater than its initial `$from`. These series-validity conditions — a non-advancing step, an ill-ordered bound, and (for a calendar `$repeat`) an `overflow: reject` boundary within the enumerable series (§14.7) — are all checked eagerly at the transition that established the series, never deferred to a temporal read.

When `$repeat` evaluates to `none`, the source produces one interval using the series bounds.

An unbounded recurring collection MUST be read through a bounded temporal selector such as `.$at` or `.$between`. The checker rejects an expression requiring enumeration of an infinite series.

### 14.6 Inferred identity

A source-backed collection MAY omit `$key`.

```text
one interval per source    source view identity
repeated intervals         source view identity + $from
```

Using `$from` makes each period's identity meaningful and stable under ordinary reads:

```text
[subscription identity, period start]
```

The source identity includes its complete source row chain. Composite components are flattened in order.

A custom key MAY use declared output fields and structural bindings:

```hjson
"$key": ["$source.external_id", "$from"]
```

The custom key MUST be unique for every generated row.

### 14.7 Period values

`period` supports fixed and calendar recurrence as ordinary table data.

```hjson
"plans": {
  "$key": "id"
  "id": "text"
  "credits": "decimal"
  "period": "period?"
}
```

Example rows:

```hjson
"weekly": {
  "credits": "30"
  "period": "P7D"
}

"monthly_paris": {
  "credits": "100"
  "period": {
    "months": 1
    "zone": "Europe/Paris"
    "overflow": "clamp"
  }
}

"quarterly_utc": {
  "credits": "400"
  "period": {
    "months": 3
    "zone": "UTC"
    "overflow": "clamp"
  }
}

"lifetime": {
  "credits": "1000"
  "period": "= none"
}
```

Fixed periods add exact elapsed duration. Calendar periods apply their fields in the declared time zone using the package's pinned time-rule data. `overflow` controls dates absent from the destination month; `clamp` chooses its final valid day, while `reject` fails the boundary. Annex A defines the complete period value.

An `overflow: reject` boundary is validated eagerly, at the source transition. When a recurrence boundary of an enumerable series — a series whose upper bound is finite, or the prefix of an unbounded series that a bounded temporal selector requires (§14.5) — lands on a calendar date missing from its destination month, the transition that established that series (the source-row insert or edit, or a change to the referenced `period` data) is rejected at admission. The overflow is never deferred to a later temporal read: a committed source-backed bucket never holds a finite series that would, on enumeration, produce an `overflow: reject` boundary. An unbounded series is read only through a bounded selector (§14.5), so an overflow beyond every committed bound is caught when that selector first enumerates the offending boundary, not silently admitted.

Because the period belongs to the plan row, one bucket collection handles weekly, monthly, quarterly, lifetime, and custom subscriptions together.

### 14.8 Accounting periods without quantity

A source containing intervals and no `$quantity` partitions spends instead of limiting them:

```hjson
"$limits": {
  "ledger": {
    "$sources": {
      "exercise": ".exercises"
    }
  }
}
```

Each entry is assigned to the active exercise at its `$time`. Closing a period is an ordinary mutation that records a boundary and rejects later changes targeting the closed interval.

---

<a id="meters"></a>

## 15. Meters

A meter atomically relates capacity pools to spending rows and records how each admitted spend was funded. The same model covers credits, quotas, overlapping subscriptions, hierarchical limits, and accounting allocation while preserving every accepted funding decision in history.

### 15.1 Meter model

Meters wire together three declarations:

```text
$consumes   a collection's rows spend from a named meter
$limits     an ancestor row enforces a named meter
$sources    views supplying capacity or accounting intervals
```

`$limits` maps meter names to meter declarations. Within one declaration, `$sources` maps stable source labels to pool views; the label becomes part of funding identity. `$order` lists successive pool comparison keys from highest to lowest priority.

A meter begins with zero capacity. Pool `$quantity` and spend `$amount` are exact decimal quantities and MUST be non-negative. Zero is valid and produces no funding rows. An admitted spend receives capacity only from declared eligible sources.

This non-negativity is enforced eagerly, at the transition that would establish the offending value, never lazily at a later meter read. A transition whose committed state would project a pool `$quantity` below zero — a source row whose projected capacity is negative — is rejected at admission of that transition, so committed state never contains a negative projected pool. Likewise a spend whose `$amount` evaluates below zero is rejected when the spend is admitted. A conforming runtime therefore never observes a negative pool at read time: the invariant holds in every committed state (§22.1).

#### Simple credits

```hjson
"users": {
  "$key": "id"
  "id": "uuid = uuid()"

  "topups": {
    "$key": "id"
    "$bucket": { "$until": ".expires_at" }

    "id": "uuid = uuid()"
    "amount": "decimal"
    "expires_at": "timestamp? = none"
  }

  "spends": {
    "$key": "id"
    "$consumes": "credits"

    "id": "uuid = uuid()"
    "amount": "decimal"
    "occurred_at": "timestamp = now()"
  }

  "$limits": {
    "credits": {
      "$sources": {
        "topup": ".topups { $quantity: .amount }"
      }
      "$order": ["$until"]
    }
  }
}
```

The source projection assigns the structural role `$quantity`. Because `topups` is bucketed, pool identity and `$from`/`$until` come from the source automatically.

A spend uses the pool instances active at its spend time. By default:

```text
$amount = .amount
$time   = .occurred_at
```

A scalar `$consumes` names one meter in the same package instance and uses these defaults. An object maps one or more meters in that instance to an amount expression or a configuration object. In a configuration object, `$amount` and `$time` override the structural expressions; other members are typed spend metadata available to `$eligible` and to the recorded funding result when used.

Meter source expressions are evaluated in the temporal context of the spend: their evaluation time is `spend.$time`. A bare bucketed source therefore yields the pool rows active at that instant. `.$all` is used only when a meter intentionally ignores bucket activity. An unbucketed source has `$from = $created` and `$until = none` unless its projection supplies other structural bounds.

### 15.2 Allocation

For each new or changed spend, the engine:

1. orders spends created by the same mutation statement by that statement's source-view order;
2. collects every reachable pool active at the spend time;
3. evaluates `$eligible` when present;
4. sorts pools by `$order` and then pool incarnation;
5. drains remaining capacity in that order;
6. rejects the complete transition when eligible capacity is insufficient;
7. records the funding allocation as an admission fact.

A spend never selects or mutates a pool directly.

```hjson
"$eligible": "!has(pool.feature) || pool.feature == spend.feature"
```

`pool` and `spend` are typed bindings. Structural fields use `$` names; bare projected fields are application data.

If `$order` is omitted, pools use source-name order followed by pool identity. A source view that repeats the same full pool identity contributes one pool, never multiplied capacity. Repeated occurrences MUST agree on quantity, interval, and projected funding metadata; disagreement rejects the admission.

Updating a spend provisionally releases its current allocation and allocates its complete new amount, time, and metadata against the prospective state. Deleting a spend releases its current allocation. If reallocation fails, the prior spend and allocation remain unchanged.

Changing or removing a pool never rewrites an earlier funding allocation. Current remaining capacity is the non-negative remainder of the pool's current quantity after allocations held by extant spend rows. A bucketed spend keeps its allocation while inactive because inactivity changes view membership rather than row existence; deletion releases it. Reducing or removing a pool can reduce future availability to zero but cannot revoke an admitted spend. Increasing it adds future availability.

### 15.3 Heterogeneous overlapping subscriptions

Plans:

```hjson
"plans": {
  "$key": "id"
  "id": "text"
  "credits": "decimal"
  "period": "period?"
  "price": "decimal"
}
```

Subscriptions:

```hjson
"subscriptions": {
  "$key": "id"
  "id": "uuid = uuid()"
  "account": { "$ref": "/accounts" }
  "plan": { "$ref": "/plans" }
  "starts_at": "timestamp"
  "ends_at": "timestamp? = none"
}
```

Derived credit periods:

```hjson
"credit_periods": {
  "$bucket": {
    "$source": ".subscriptions"
    "$from": "$source.starts_at"
    "$until": "$source.ends_at"
    "$repeat": "/plans[$source.plan].period"
  }

  "credits": "= /plans[$source.plan].credits"
  "price": "= /plans[$source.plan].price"
}
```

Account meter and spend mutation:

```hjson
"accounts": {
  "$key": "id"
  "id": "uuid = uuid()"

  "$limits": {
    "credits": {
      "$sources": {
        "subscription": '''/credit_periods[:p | p.$source.account == .] {
          $quantity: .credits,
          price
        }'''
      }
      "$order": ["$until", "price"]
    }
  }

  "spends": {
    "$key": "id"
    "$consumes": "credits"

    "id": "uuid = uuid()"
    "amount": "decimal"
    "occurred_at": "timestamp = now()"
  }

  "$mut": {
    "consume": [
      "spend = .spends + { amount: @amount }"
      "return spend { id, amount, occurred_at, funding }"
    ]
  }
}
```

At one spend time, the account MAY have overlapping weekly, monthly, quarterly, and lifetime pools. With remaining capacities:

| Pool | Remaining | `$until` |
|---|---:|---|
| weekly | 30 | June 25 |
| monthly | 100 | July 1 |
| quarterly | 400 | July 15 |
| lifetime | 1000 | `none` |

A spend of `150` receives:

```text
weekly       30
monthly     100
quarterly    20
```

The lifetime pool follows every finite expiry because ascending optional order places `none` last.

The returned funding view contains the fixed allocation:

```hjson
[
  { source: "subscription", pool: [weekly-subscription, weekly-start], amount: "30" }
  { source: "subscription", pool: [monthly-subscription, monthly-start], amount: "100" }
  { source: "subscription", pool: [quarterly-subscription, quarterly-start], amount: "20" }
]
```

Changing or removing a plan or subscription affects future admissions and current pool views. Earlier funding remains attached to its spend even when the source row later disappears.

The frozen admission fact retains the enforced meter level, source name, pool identity, allocated quantity, and interval bounds so replay and audit are deterministic; the observable funding view exposes only source, pool, and amount (§15.6). Later source edits never rewrite that recorded allocation. Applications that want subscription terms frozen at purchase copy the period, quantity, and price onto the subscription row; applications that keep plan references intentionally let later plan changes affect future periods and admissions.

### 15.4 Hierarchical limits

The same meter name MAY appear at several lexical ancestor rows inside one package instance. A spend MUST clear every declaration reached through that ancestor chain:

```text
account credits
company credits
parent-company credits
```

Each level records its own funding rows. A level without that meter adds no constraint. Package-instance boundaries end this implicit lookup.

### 15.5 Module boundaries

A meter's capacity and spending domain is the package instance that owns its declaration. Cross-module consumption uses an ordinary mutation exposed by that owner, as defined in [Stateful services across module boundaries](#1310-stateful-services-across-module-boundaries). The owner mutation performs allocation privately and joins the caller's work in one atomic transition.

This keeps the module boundary uniform: views expose data, mutations expose controlled state changes, and module placement creates no implicit meter relationship.

### 15.6 Meter accessors

```text
.credits.balance                         context-free current capacity
.credits.pools                           context-free eligible pool view
.credits.balance({ $time, metadata... }) capacity for a hypothetical spend context
.credits.pools({ $time, metadata... })   pools for a hypothetical spend context
spend.funding                            fixed admission allocation for that spend
```

The parameterless forms are valid when `$eligible` and source selection require no spend-specific member beyond the meter's default current time. When eligibility references `$amount`, `$time`, or named spend metadata, the accessor call MUST supply every referenced value. The result uses the same source evaluation, duplicate-pool coalescing, order, and remaining-capacity rules as admission.

`funding` is a keyed view identified by meter, enforced level, source, and pool identity. The implementation MAY materialize or reconstruct it while preserving the recorded allocation.

The observable `spend.funding` view has exactly the members `source` (text), `pool` (opaque pool identity), and `amount` (decimal). Its shape is fixed and independent of the meter's source projection. Source-projected metadata (for example `price`), the enforced level, and interval bounds participate in `$order`, `$eligible`, and the §15.2 duplicate-pool agreement, and are retained in the frozen admission fact for replay and audit, but are not members of the returned funding view; an application that needs such metadata attached to a spend copies it onto a row (§15.3).

---

<a id="host-namespaces"></a>

## 16. Host namespaces

A host namespace is a typed function and value contract implemented in Rust and registered in the Liasse context. The core namespace set remains small; packages add CBOR, COSE, OIDC, password hashing, or domain functions through explicit, versioned namespace requirements.

### 16.1 Core functions and namespaces

The standard core contains:

```text
language     arithmetic, logic, selectors, views, aggregates, assertions,
             none/has, uuid(), now()
hex          byte/text hex conversion
base64       byte/text base64 conversion
sha          standard cryptographic hashes
string       Unicode-safe text utilities
convert      checked value conversions
time         Unix time, dates, durations, periods, and time-zone operations
```

CBOR, COSE, JOSE, OIDC, password hashing, payment protocols, compression, and domain libraries are external typed namespaces.

### 16.2 Package requirements

A package declares only the non-core namespaces it uses:

```hjson
"$requires": {
  "cbor": "liasse.cbor@1"
  "cose": "liasse.cose@1"
  "password": "liasse.password@1"
  "oidc": "liasse.oidc@1"
}
```

The local key is the expression namespace. The value identifies the semantic contract and compatible major.

At load time, the context supplies a descriptor containing:

```text
namespace id and version
named value types and their canonical codecs
function names and typed signatures
effect class for each function
equality, ordering, and key eligibility for namespace types when provided
semantic interface hash
```

The package install record pins the resolved descriptor. Missing, incompatible, or ambiguous requirements reject loading before the package becomes active.

This is an explicit host dependency rather than an open language extension: every used function has a known type, effect class, and pinned contract.

### 16.3 Effect classes

A namespace function declares one of these behaviors:

```text
pure        same logical inputs produce the same output
verifier    validates untrusted input against declared keys/configuration and
            returns a typed proof or diagnostic
generated   may use randomness, clocks, or provider operations; one successful
            result is fixed for the admitted operation
```

Pure functions MAY run during views, checks, and replay. Verifiers run during external request admission. Generated functions run in mutation/write-time positions. A generated result affecting state, identity, funding, authorization, or another durable admission fact is recorded. "One successful result is fixed for the admitted operation" scopes a single generated call — its result is fixed once produced and reused verbatim on replay (§8.12) — not a field default evaluated once per row: a `uuid()` default across several rows is several evaluations, each with its own fresh result (§5.1).

A host namespace return that does not satisfy its declared result type — including a struct return carrying an undeclared member (§5.8) — is a §2.1 nonconformance: the runtime rejects the call and commits no effect, exactly as for any off-contract return. The runtime does not coerce, widen, or strip an off-contract return.

Arbitrary untracked external side effects are outside expression evaluation. Integrations represent them through explicit provider workflows and committed observations.

### 16.4 Rust registration

Conceptually:

```rust
let context = LiasseContext::builder()
    .namespace("cbor", cbor_namespace())
    .namespace("cose", cose_namespace())
    .namespace("webauthn", webauthn_namespace())
    .namespace("oidc", oidc_namespace())
    .build()?;
```

A namespace receives typed Liasse values and explicitly granted provider handles. It MAY define additional named value types when their canonical codec and semantics are part of the pinned descriptor. It does not receive unrestricted access to application state.

---

<a id="keyrings"></a>

## 17. Keyrings and key providers

A keyring is logical state governing cryptographic key versions and their lifecycle. A key provider is a host-supplied Rust implementation that owns opaque private-key handles and performs protected operations. The application defines rotation and acceptance policy while the provider maps that policy to local keystores, cloud KMS systems, HSMs, or physical keys.

### 17.1 Keyring declarations

A keyring declaration selects its provider, algorithm, and lifecycle policy. Private key material remains behind the provider.

Simple form:

```hjson
"session_keys": {
  "$keyring": {
    "$provider": "session-hsm"
    "$algorithm": "Ed25519"
    "$rotate": "P30D"
    "$retain": "P45D"
  }
}
```

More controlled policy:

```hjson
"session_keys": {
  "$keyring": {
    "$provider": "session-hsm"
    "$algorithm": "ES256"
    "$usage": ["sign"]
    "$rotate": {
      "$every": "P30D"
      "$overlap": "P2D"
      "$mode": "automatic"
    }
    "$retain": "P45D"
    "$protection": "hardware"
  }
}
```

`$usage` is the set of permitted key operations. When omitted, the checker infers the minimal operation set required by every package call site using the keyring. A duration `$rotate` is shorthand for automatic rotation at that cadence. In the object form, `$every` sets the cadence, omitted `$overlap` means zero lead time, and omitted `$mode` means `automatic`. Omitting `$rotate` disables scheduled rotation and leaves activation manual. `$retain` controls how long a retired version remains accepted for verification; when omitted, retired public versions remain accepted until explicit revocation or destruction. `$protection` states the required provider protection class; omission adds no requirement beyond the provider's declared capabilities.

The declaration defines observable policy. The provider and runtime choose physical key storage, processes, and devices.

### 17.2 Public keyring view

A keyring exposes public metadata:

```text
ring.$current      active version metadata
ring.$accepted     versions accepted for verification
ring.$public       public key values for accepted versions
ring.$versions     all retained version metadata
```

A version exposes:

```text
id
algorithm
public key
created_at
activated_at?
retired_at?
revoked_at?
provider metadata safe for application views
attestation?
```

Private key bytes and provider credentials are never application values.

### 17.3 Version lifecycle

```text
pending -> active -> retired -> destroyed
                     \-> revoked
```

- `pending` has a provider handle and verified public metadata;
- `active` is selected for new operations;
- `retired` remains accepted according to `$retain`;
- `revoked` is rejected immediately;
- `destroyed` has completed provider destruction and retains only allowed audit metadata.

At most one signing version is active for a keyring at one admitted state position. A package surface that requires an active keyring becomes available only after that keyring has one active version. During initial `create`, automatic mode generates and activates the first version as part of bootstrap; manual mode requires the host to bind and activate one before the dependent surface is enabled.

### 17.4 Rotation

Automatic rotation performs this logical sequence:

1. request a new provider key satisfying the policy;
2. read and validate its public key and provider metadata;
3. record and expose a pending public version at the configured `$overlap` lead time;
4. atomically activate it and retire the prior active version;
5. retain prior public versions while tokens or signatures MAY remain valid;
6. disable and destroy provider versions according to policy.

Rotation transitions are system commits with no actor. A runtime MAY schedule them or perform a due rotation before the next operation. The resulting logical order and key selection are identical.

A manual policy exposes an operator mutation that binds an externally created provider handle, validates its public metadata, and activates it through the same transition.

### 17.5 Key provider trait

A provider is registered under the name used by `$provider`:

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

Providers MAY add capability-gated operations such as decrypt, key agreement, wrapping, or MAC. Namespace descriptors state which provider capability they require.

`KeyHandle` is opaque outside the provider/runtime boundary.

### 17.6 Provider capability checks

Loading a keyring validates that its provider advertises the required:

- algorithm and key type;
- operation set;
- automatic generation or external binding mode;
- requested hardware/protection class when declared;
- disable, destroy, and attestation behavior required by policy.

An incompatible provider rejects package loading or keyring activation with a capability diagnostic.

The trait can wrap local encrypted storage, operating-system keystores, cloud key services, PKCS#11 devices, network HSMs, smart cards, or application-specific infrastructure.

### 17.7 Restricted namespace access

A COSE namespace MAY sign through a keyring:

```text
cose.sign(/session_keys, claims)
```

The namespace receives a restricted signer for the current active version. It receives no private bytes and no unrestricted provider object.

Verification uses the accepted public versions:

```text
cose.verify(/session_keys, token)
```

The namespace result includes the verified key-version identity so authentication policy can reject revoked or disallowed versions.

### 17.8 Direct session token construction

A public login mutation can construct and return its own token:

```hjson
"login": [
  "session = /sessions + { account: @account, expires_at: now() + time.duration('P30D') }"
  '''token = cose.sign(/session_keys, {
    auth: 'session',
    session: session.$key,
    expires_at: session.$until
  })'''
  "return { auth: 'session', token }"
]
```

The package controls claims, session mapping, token lifetime, and accepted key versions. The registered namespace controls the pinned token format and cryptographic encoding. The provider controls the private operation.

### 17.9 Provider failures

A provider failure before admission rejects the affected operation and commits no application effect. When scheduled rotation cannot create or validate a replacement, the current version remains active and the runtime reports the overdue rotation through health and diagnostics. Operations using that version continue when the provider can perform them; an unavailable signing operation rejects the requesting mutation.

A provider result used by a successful mutation is fixed when it enters committed state or another durable admission fact. Key activation, retirement, revocation, destruction, and provider-status observations are explicit logical transitions. Ephemeral return values remain transport output.

---

<a id="blobs"></a>

## 18. Blobs

A blob is a typed descriptor for binary content whose bytes MAY live outside the main state store. A blob connector implements one external storage system. The logical model keeps identity, placement, access, integrity, retention, and billing observable while providers perform physical transfer and storage.

### 18.1 Blob values

A `blob` field contains a descriptor for binary content:

```hjson
{
  "$sha512": "ab31..."
  "$bytes": "184320"
  "$media": "application/pdf"
  "$name": "invoice-113.pdf"
}
```

`$sha512`, `$bytes`, and `$media` are required; `$name` is optional. `$sha512` is 64 SHA-512 bytes encoded as lowercase hexadecimal, `$bytes` is a non-negative integer byte count, and `$media` is a canonical media type. The content hash identifies and verifies the byte sequence. The complete descriptor is the application value: two descriptors MAY name the same content while carrying different media or filename metadata.

Expressions read descriptor metadata and placement state. Raw content crosses the connector boundary through upload and fetch operations.

### 18.2 Accepted blob types

```hjson
"pdf": {
  "$type": "blob"
  "$max_bytes": "10485760"
  "$media": ["application/pdf", "image/jpeg", "image/png"]
}
```

`$max_bytes` is an inclusive content-size limit. The `$media` array is the set of accepted media types. Media type and subtype are compared case-insensitively after canonical lowercasing; parameters are sorted by lowercase parameter name and compared exactly when the declaration includes them. A declaration without parameters accepts the same type/subtype with any parameters. Wildcards are absent from this core form.

A blob parameter is accepted after streaming verification confirms:

- byte limit;
- accepted media;
- exact byte count;
- SHA-512 content hash;
- every verified copy required by one complete branch of the current placement plan.

A failure rejects the containing call before its state transition is admitted.

### 18.3 Connectors and store rows

A connector is a Rust provider registered in the Liasse context. A store row selects that connector and supplies application-visible configuration:

```hjson
"stores": {
  "$key": "id"
  "id": "text"
  "connector": "text"
  "params": "json"
  "enabled": "bool = true"
}
```

Secrets remain in host configuration or vault handles resolved by the connector. Store rows MAY be scoped to companies, users, or modules like ordinary data.

A store row selects its connector eagerly. The `connector` value a store row carries is resolved and validated against the registered connectors at admission of the transition that writes the row — not deferred to the first blob upload that routes to it. When a store row is reachable by a declared placement (§18.4), admitting a write that names a connector the context has not registered, or whose advertised capabilities cannot satisfy the placement and client behavior it is routed by, is rejected at that transition exactly as an unmet load-time requirement is (§18.12, §2.1). Committed state therefore never contains a placement-reachable store row bound to an unresolvable connector, and no blob can be routed to one. This is the runtime-write analogue of the §18.12 load-time validation, which resolves the connectors named by store rows and placement present at activation.

### 18.4 Placement policy

`$blob_storage` applies to blob fields below the declaration. The nearest declaration wins.

```hjson
"$blob_storage": {
  "$in": "/stores['primary'] | /stores['archive']"
  "$serve": "/stores['primary']"
}
```

Placement grammar:

```text
view                         verified in every store yielded by the view
{ $all: [placement, ...] }   every branch
{ $any: [placement, ...] }   any branch; first branch is the new-write plan
{ $copies: n, $of: view }    any n verified stores from the ordered view
```

Examples:

```hjson
"$in": { "$any": ["/stores['new']", "/stores['old']"] }
```

```hjson
"$in": {
  "$all": [
    { "$any": ["^.storage.stores[:s | s.enabled]", "/stores['main']"] }
    { "$copies": 2, "$of": "/stores[:s | s.region == 'eu']" }
  ]
}
```

In `$all`, every array element is required simultaneously. In `$any`, elements are alternatives in preference order. Existing verified copies satisfy the policy when any branch is satisfied; a new write chooses the first branch whose complete requirements can currently be fulfilled, and rejects when none can.

`$copies` evaluates its store view in declared order, removes repeated store identities by first occurrence, and chooses the first `n` currently writable capable stores for a new write. At steady state, any `n` verified distinct stores from that view satisfy the branch; fewer than `n` available stores reject the plan.

Placement order is flattened depth-first and left-to-right: a view contributes its row order, `$all` and `$any` contribute branches in array order, and `$copies` contributes its source-view order. Repeated store identities are removed by first occurrence. `$serve` controls preferred read order and defaults to that flattened placement order.

### 18.5 Logical placement state

Each blob occurrence exposes:

```text
blob.$sha512
blob.$bytes
blob.$media
blob.$name?

blob.$policy.$in
blob.$policy.$serve

blob.$placement[store]
blob.$stored
blob.$satisfied
blob.$surplus
```

A placement row has a state such as:

```text
pending | copying | verified | corrupt | draining
```

and MAY expose timestamps, verification status, and copy progress.

`$stored` contains verified stores. `$satisfied` evaluates the placement policy over them. `$surplus` contains verified copies outside the currently required policy.

These are logical observations recorded by the engine. The implementation MAY maintain them in tables, documents, indexes, or another internal form.

### 18.6 Reconciliation

A background reconciler converges verified placement toward policy:

1. choose a verified source;
2. copy bytes through connectors;
3. verify hash and size at the destination;
4. record the destination as verified;
5. drain surplus copies after retention policy allows it.

Reconciler observations are actorless system transitions. They use the same type and transition checks as other state changes.

A corrupt observation demotes that copy and triggers repair from another verified holder. The descriptor continues to identify the intended content.

### 18.7 Transactional uploads

Blob bytes are staged before mutation admission. At the mutation's final serial position, the runtime re-evaluates the current target surface, authentication, accepted blob type, and complete placement policy. The mutation commits only after one complete policy branch is satisfied by verified landed copies.

The call has one logical sequence:

1. resolve the target surface and current authentication context;
2. obtain a provisional upload plan;
3. stream and stage bytes while enforcing limits and hashing;
4. bind the verified descriptor to the mutation parameter;
5. re-evaluate the current acceptance and placement declarations at admission;
6. create every required verified copy for one complete policy branch;
7. admit the mutation;
8. advance authorized live views;
9. return the committed result.

Staged transport objects left by an interrupted or rejected upload are outside application state and MAY be swept after a host-defined grace period. A connection failure follows the operation-status semantics in Section 12.3.

### 18.8 Fetch plans

Visibility of a `blob` value through a currently authorized surface grants fetching the exact bytes identified by that descriptor. A projection exposing only selected metadata grants that metadata and no blob fetch. The runtime re-evaluates the caller's authentication, scoped role membership, surface projection, descriptor occurrence, and current verified holders when issuing or refreshing a fetch plan.

The plan contains accessible verified holders in `$serve` order and connector-specific access data. A client MAY probe holders, use ranged and resumable reads, combine ranges, compute SHA-512 while streaming, and replace ranges received from a mismatching source. A successful fetch returns exactly the bytes identified by `$sha512`.

When the runtime performs the fetch on the caller's behalf, it MUST attempt each accessible verified holder in `$serve` order and apply the §18.9 fetch verification to each. The fetch succeeds — returning exactly the bytes identified by `$sha512` — if and only if at least one accessible verified holder delivers hash-clean content. If none does, the fetch fails with an `unavailable` outcome, distinct from the `denied` outcome of a metadata-only or revoked projection; the fetch does not block on reconciliation (§18.6), so a fetch retried after repair MAY then succeed. The client-side probing, ranged and resumable reads, range combination, and mismatching-range replacement remain permitted transfer strategies but do not alter this outcome as a function of the current holder state.

### 18.9 Integrity

Hash verification occurs:

```text
ingress       while bytes stream and after landing
copy          before a destination becomes verified
scrub         according to runtime policy
fetch         before delivering a successful result
```

Store objects MAY be content-addressed by SHA-512, allowing deduplication and idempotent copying. The specification requires content behavior and verification, independently of physical object naming.

### 18.10 Retention and vacuum

Live descriptors and retained history pin content according to storage policy. Vacuum MAY remove bytes only when every applicable live, history, placement, and grace requirement permits it.

History MAY retain a descriptor after physical bytes are released. Its content hash and metadata remain available for integrity and audit.

### 18.11 Billing and observations

Application billing uses committed logical placement rows and descriptor sizes:

```hjson
"s3_bytes": "= sum(.uploads[:u | /stores['s3'] in u.file.$stored].file.$bytes)"
```

Connector-reported physical usage is a host observation for reconciliation and auditing. It becomes application data only through an explicit committed observation.

### 18.12 Connector contract

A connector descriptor advertises capabilities such as:

```text
stream upload/download
presigned upload/download
range reads
server-side copy
checksum support
delete
physical usage observation
```

Representative Rust integration:

```rust
context.blob_connector("s3", Arc::new(S3Connector::new(...)));
context.blob_connector("fs", Arc::new(FileConnector::new(...)));
```

Loading validates connector capabilities required by declared placement and client behavior. Temporary connector failure rejects or delays the affected operation while preserving committed application state.

---

## Part IV — History and application lifecycle

<a id="history"></a>

## 19. History, artifacts, and reconciliation

History preserves committed Liasse state for export, recovery, rollback, branching, and reconciliation. It is available through host lifecycle operations. Application expressions, views, roles, and mutations observe the selected live state.

### 19.1 History ownership and composition

Every package instance owns one independent history covering:

- its definition, resources, configuration, and boundary bindings;
- its writable state;
- installation, mounting, binding, disabling, and removal of its direct child instances;
- the logical effects of committed transitions within that boundary.

Each child instance owns its own state and history. A parent history may select a child incarnation and one of that child's history points, while child-owned deltas remain in the embedded child artifact. A parent export includes the required child artifacts recursively.

A transaction affecting several package instances is represented in each affected history with one shared transaction identity. Importing the complete parent artifact preserves that transaction's atomic grouping. Extracting one child artifact preserves the child's independent history and represents participants outside its boundary as external transaction context.

### 19.2 Capture and materialization

History construction follows committed transitions independently of write admission. Independent writes may be captured concurrently. The implementation may use journals, logical decoding, checkpoints, deltas, or another lossless mechanism.

Before export, rollback, or reconciliation, the implementation MUST materialize a stable history through the selected committed boundary. The resulting order MUST preserve every precedence relation established by admission and MUST reproduce the committed state. Truly unordered changes may appear in either order. Once a history point has been exported, its identity and relative position remain stable.

History points, combined deltas, compressed archives, indexes, and checksums MAY be produced asynchronously or during export. Write coherence comes from admission. History materialization uses per-transition capture and assigns stable points independently of the write path.

### 19.3 History points, lineages, and branches

A **history point** identifies one exact retained state of one package instance. A **lineage** is an ordered continuation from genesis or from an earlier point. Position identifiers order points only within their lineage; timestamps remain ordinary recorded values.

Rollback selects an earlier point. The displaced continuation remains available as an alternate lineage. Continuing from the selected point extends a new lineage. Imported divergence and successful merge reconciliation likewise create lineages while preserving their source histories.

A **composition point** selects one coherent point for the parent instance and each currently mounted direct child instance. Composition points contain child selections and boundary bindings; each child's owned state remains in that child's history. Exporting a parent records the selected composition recursively.

`$history` declares the minimum recoverable range for one package instance:

```hjson
"$history": "all"

"$history": {
  "$minimum": "P10Y"
}
```

`all` preserves the complete retained history. `$minimum` requires every active lineage to retain a restorable range reaching at least that duration before its head, or to its origin when younger. Omitting `$history` is equivalent to `all`. A host MAY retain more. Explicit erasure remains the operation that removes selected historical payload.

### 19.4 Reversible compaction

A contiguous range of one lineage MAY be replaced in active history by one combined forward delta and one combined reverse delta. The forward delta reconstructs the range's final state from its base; the reverse delta restores the base from its final state.

The extracted details are stored in a compressed archive. Expanding that archive MUST reproduce every replaced transition, intermediate point, established order, package transition, and cross-module transaction association. Compacted segments MAY later be combined into larger segments while preserving the same behavior.

Compaction changes the portable representation of retained history. It preserves point identity, lineage ancestry, forward and reverse reconstruction, module ownership, and reconciliation results.

### 19.5 Recursive `.liasse` artifact

Every `.liasse` artifact represents one concrete application or module instance and uses this ZIP64 structure:

```text
mimetype
manifest.json
liasse.json
resources/
state/current.cbor.zst
history/index.json
history/segments/
history/archives/
history/definitions/
blobs/
modules/<instance-incarnation>.liasse
```

`mimetype` contains:

```text
application/vnd.liasse+zip
```

`liasse.json` and `resources/` contain the definition active at the selected point. Historical definitions and their resources appear below `history/definitions/` when the included history requires them.

`state/current.cbor.zst` contains the selected complete state owned by this instance, including its configuration, bindings, direct child mounts, row incarnations, and refs. Child-owned state resides in each selected child artifact.

`history/index.json` identifies the selected lineage and point, retained lineages, composition points, checkpoints represented by state entries, segment coverage, archive coverage, and package definitions required by those points.

`history/segments/` contains forward and reverse portable deltas or checkpoints. `history/archives/` contains compressed details extracted by reversible compaction. `blobs/` contains owned blob bytes selected for inclusion.

Every entry below `modules/` is a complete `.liasse` artifact for one particular direct child-module instance. The filename uses the child instance incarnation. Extracting that entry yields an independently valid artifact. Multiple installations of the same definition therefore appear as separate child artifacts.

The parent `manifest.json` maps current mount names to child incarnations and selected child points. It also inventories historically contained child artifacts required by the included parent history, including children absent from the selected current composition.

Its required structure is:

```hjson
{
  "format": 1
  "instance": "<instance-incarnation>"
  "selected": {
    "lineage": "<lineage-id>"
    "point": "<point-id>"
  }
  "definition": {
    "identity": "sha256:..."
    "path": "liasse.json"
  }
  "state": {
    "path": "state/current.cbor.zst"
    "sha256": "..."
  }
  "history": {
    "path": "history/index.json"
    "sha256": "..."
  }
  "modules": {
    "<mount-name>": {
      "instance": "<child-incarnation>"
      "artifact": "modules/<child-incarnation>.liasse"
      "selected": { "lineage": "<lineage-id>", "point": "<point-id>" }
    }
  }
  "included_modules": {
    "<child-incarnation>": {
      "artifact": "modules/<child-incarnation>.liasse"
      "sha256": "..."
    }
  }
  "entries": {
    "<archive-path>": {
      "media": "<media-type>"
      "sha256": "..."
    }
  }
}
```

`modules` describes the selected direct mounts. `included_modules` inventories every direct child artifact required by the exported state or retained parent history. `entries` covers every required direct archive *leaf* entry — `mimetype`, `liasse.json`, `state/current.cbor.zst`, and `history/index.json`, together with every present `resources/`, `history/segments/`, `history/archives/`, `history/definitions/`, and `blobs/` section — other than `manifest.json` itself, which cannot checksum itself, and the nested child-module artifacts under `modules/`, which `included_modules` inventories. Its member name is the exact archive path. Where a covered entry's checksum also appears in a role member (`state`, `history`), the two MUST be equal. Additional members are invalid for format version `1`.

### 19.6 Portable history records

State snapshots, deltas, and archived details use the canonical artifact encoding defined in Annex D. Portable deltas describe changes to stored logical state by stable declaration identity, row incarnation, field or structural coordinate, and typed before/after value. Computed values and views are reconstructed from the definition and stored state.

A portable delta also represents definition, configuration, binding, and direct child-mount transitions owned by that instance. One coordinate appears at most once in a combined delta; its `before` value is the value at the range base and its `after` value is the value at the range tip.

`history/index.json` has this required structure:

```hjson
{
  "format": 1
  "selected": { "lineage": "<lineage-id>", "point": "<point-id>" }
  "lineages": {
    "<lineage-id>": {
      "origin": "genesis"
      "head": "<point-id>"
      "ranges": {
        "<range-id>": {
          "base": "<point-id>"
          "tip": "<point-id>"
          "segment": "history/segments/<range-id>.cbor.zst"
          "archive": "history/archives/<range-id>.cbor.zst"
        }
      }
    }
  }
  "compositions": {
    "<point-id>": {
      "modules": {
        "<mount-name>": {
          "instance": "<child-incarnation>"
          "lineage": "<lineage-id>"
          "point": "<point-id>"
        }
      }
    }
  }
}
```

A lineage created from an earlier point uses `{ "lineage": "...", "point": "..." }` as `origin`. `ranges` partitions the retained points of that lineage into non-overlapping contiguous ranges. `archive` is omitted when the segment already contains full detail. `compositions` is present only for points that select or change direct child composition.

The artifact manifest records checksums for every required entry and nested artifact. Checksums verify represented artifact content. History ancestry and point identity are represented explicitly; checksums serve artifact-content verification.

### 19.7 Export boundary and scope

An export selects one committed application boundary. The root state, retained histories, composition point, and recursively selected child states MUST describe that same boundary. Later writes belong to a later export boundary and MAY continue while the artifact is constructed.

Exporting a parent recursively includes:

- its selected state and owned history;
- every currently mounted child artifact;
- every historically contained child artifact required to reconstruct the selected history;
- the same closure recursively for all descendants;
- active and historical definitions and resources required by the selection;
- selected owned blob contents.

Exporting one child produces the same artifact found inside the parent export, together with its descendants and external boundary requirements. The child artifact carries its external boundary requirements, while parent and peer private state stay within their owning artifacts.

The host MAY select the active lineage, additional lineages, a recovery range, or complete retained history. The artifact manifest states the included range and whether every represented point is fully restorable.

### 19.8 Import and automatic reconciliation

Import verifies the complete recursive artifact and compares each matching instance incarnation with local retained history:

```text
same point                         already synchronized
local point precedes incoming      fast-forward available
incoming point precedes local      rollback available
shared point followed by divergence three-way merge
no shared point                    unrelated import policy required
```

Fast-forward applies the incoming continuation. Rollback restores the selected earlier point and preserves the displaced future as another lineage. Reconciliation proceeds independently within each module boundary, then validates and activates the complete recursive composition atomically.

Parent reconciliation controls parent-owned state and direct child mounts. Matching child incarnations reconcile inside their own artifacts. Competing child incarnations selected for the same mount produce a parent-level conflict.

An import policy selects which automatic movements may activate, including fast-forward, rollback, merge, branch creation, and unrelated replacement. Valid imported alternatives MAY remain available without becoming active.

### 19.9 Automatic merge and manual correction

Automatic merge uses the latest shared history point as its base and compares the base, local, and incoming logical states after bringing them to the selected compatible definition.

The merge accepts an unambiguous combined result, including a change made on one side, equal results reached on both sides, and compatible changes to separate logical coordinates. It reports conflicts for incompatible field values, deletion against modification, competing rekeys or identities, competing module mounts, incompatible definition or boundary changes, and any combined result that fails ordinary Liasse validation.

Each reported conflict names its coordinate as a §D.3 application address relative to the model root, so a host correction resolves it by that address: a keyed-collection conflict at the conflicted row's display path, and a §8.2 root-singleton member conflict at that member's declaration-name address (`.flag`, equivalently `/flag`), with no collection or key wrapper. Internal reserved storage names never appear in a reported coordinate.

A failed merge returns a reconciliation plan containing the base, local, incoming, proposed result, conflicts, and affected module boundaries. A host correction function may select or provide valid values and resolve direct child-mount choices within each affected boundary. Liasse then validates the complete prospective composition under the ordinary type, ref, deletion, uniqueness, check, authorization, bucket, meter, blob, migration, and interface rules.

Activation succeeds atomically and records the accepted correction in a new lineage preserving both source histories. Failure leaves the active composition unchanged and keeps the reconciliation plan available for another correction attempt. The Rust API shape is implementation-defined.

### 19.10 Round-trip and external effects

Restoring an artifact and exporting the same instance boundary, selected points, and retained history reproduces the same definitions, resources, owned logical states, lineages, transaction associations, module-instance closure, and reconciliation transitions. Segment boundaries, compression levels, indexes, ZIP metadata, and internal storage layout may differ.

History movement restores logical Liasse state. External payments, messages, hardware actions, and provider-side effects are reconciled through their committed application records and provider contracts.

<a id="evolution"></a>

## 20. Package evolution and migrations

A migration is a package-declared transformation from the active logical model to a target package model. Compatibility determines which changes remain within one package major. Together they make upgrades atomic, checked, and portable across implementations.

### 20.1 Schema migration

A target field MAY identify its previous source. The members of the expanded declaration form one local migration mapping: `$from` names the old field or collection, `$as` transforms its old value bound as `.`, and `$back` optionally defines the inverse.

```hjson
"display_name": {
  "$type": "text"
  "$from": "name"
  "$as": "string.trim(.)"
}
```

Without `$as`, the compatible value is copied. A value is a *compatible copy* only when its canonical Annex-A wire form is directly decodable under the target field's type through the §19 portable codec; the copy preserves representation and performs no cross-type value coercion. A change of scalar base type therefore copies only values whose existing representation already satisfies the target type — for example every `int` decodes as `decimal`. A value that does not — a `decimal` with a fractional scale into `int`, or any `text` into `int` — is not compatible; absent an explicit `$as` transform producing the target type it is rejected exactly as an unpopulated required field is. Lossy or value-dependent conversions (`decimal`→`int`, other narrowings) MUST be requested explicitly with `$as` — checked conversions live in the `convert` namespace (§16.1) — and are never implicit, so a migration's success never depends on a particular stored value or on an unpinned decimal scale spelling (Annex A.1). The same shorthand renames a collection:

```hjson
"clients": {
  "$from": "customers"
  "$key": "id"
  "id": "text"
  "name": "text"
}
```

For splits, merges, one-to-many changes, or coordinated collection transforms, the target package MAY declare a direct migration program for an exact source package version:

```hjson
"$migrations": {
  "1.4.0": [
    ".people = $old.users { id, display_name: string.trim(.name) }"
    ".emails = $old.users[:u | has(u.email)] { user: .id, email: .email }"
  ]
}
```

`$old` is the complete read-only state under the source package. `.` is the prospective target state after compatible declarations and local `$from` mappings have been copied. The array is one ordered atomic migration program using mutation statements over the target state; it MAY read any `$old` view and MUST use deterministic pure functions. The map key is the exact active source package version. A runtime MAY compose a sequence of package versions only when every adjacent target package supplies a migration from the preceding active version.

Migration order is: compatible same-identity copy, local `$from` mappings in dependency order, then the selected package-level statements in array order. Source values absent from the target remain available in preceding history and exports but are absent from live target state. A migration that needs to preserve them live must copy them explicitly.

The complete prospective target is checked under ordinary keys, refs, uniqueness, checks, buckets, meters, modules, and interfaces before the package update commits. This is the same full admission suite an ordinary transition runs, applied eagerly to the whole migrated state: a migration commits only when the prospective target satisfies every invariant. In particular the meter suite (§15) re-funds every migrated spend against the migrated pools and rejects the migration when eligible capacity is insufficient or a projected pool `$quantity` is negative, and the bucket suite (§14.5, §14.7) rejects a migrated source-backed series that is non-advancing, ill-bounded, or reaches an `overflow: reject` boundary. A migration that would leave any of these invariants violated is rejected as a whole and the prior package remains active (§20.3).

### 20.2 Reversible transforms and downgrade

A transform MAY declare an exact inverse:

```hjson
"encoded_name": {
  "$type": "text"
  "$from": "name"
  "$as": "base64.encode(string.bytes(.))"
  "$back": "string.from_bytes(base64.decode(.))"
}
```

For every actual migrated value `x`, the engine verifies:

```text
$back($as(x)) == x
```

A failed round trip rejects the complete migration.

A downgrade loads the older package and applies an explicit direct migration or available exact inverses. When the older shape cannot represent the current live values and no declared downgrade transform preserves them, the downgrade is rejected. Prior values remain available in history; history order remains unchanged.

### 20.3 Compatibility and update checking

Within one package major, minor and patch releases MUST preserve or widen the compatibility surface. Breaking changes use a new major. The runtime validates the prospective model, migrations, interfaces, namespace contracts, state constraints, and retained history before admitting the update.

The full compatibility algorithm is specified in Annex E.

---

<a id="deletion"></a>

## 21. Deletion and erasure

Deletion removes a row from live state according to every inbound reference policy. Erasure additionally scrubs retained payload bytes while preserving the verifiable structure of history. The separate operations keep ordinary lifecycle removal concise and make irreversible privacy-sensitive work explicit.

### 21.1 Deferred delete policy

A ref MAY omit `$on_delete` while its target cannot be deleted by any declaration in the target's owning module. The omission is an unresolved decision rather than an implicit policy.

The checker computes possible deletion transitively across mutation calls. A declaration introduces a deleting capability when it MAY execute:

```text
collection - key
-row_source
collection = replacement_view
erase(row)
```

Any operation that can remove an occurrence from a ref's declared target relation is treated the same way. This includes migration, collection replacement, bucket lifecycle, keyed-view membership change, module disable, interface withdrawal, module update, and uninstall.

Before such a capability becomes active, every possible inbound ref MUST declare one of:

```text
restrict   reject deletion while the ref exists
cascade    delete the containing row or set member
none       clear this optional ref
= patch    patch the containing row
```

`none` is valid only for an optional ref and expands to a patch assigning `none` to that referencing field. The patch form remains available when deletion must change several fields or compute replacement values.

Example: the refs MAY begin compactly while projects have no deleting mutation:

```hjson
"tasks": {
  "$key": "id"
  "id": "uuid = uuid()"
  "project": { "$ref": "/projects" }
}
```

Adding project deletion activates the decision, so the ref MUST be completed in the same package update:

```hjson
"tasks": {
  "$key": "id"
  "id": "uuid = uuid()"
  "project": {
    "$ref": "/projects"
    "$on_delete": "cascade"
  }
}

"$mut": {
  "delete_project": ".projects - @id"
}
```

The package or module update that introduces the deleting capability is rejected as a whole when any inbound ref remains undecided. The diagnostic names the deleting declaration and each unresolved ref.

This requirement is local and deferred because a module owns its private data and its mutation declarations. Refs crossing module boundaries declare `$on_delete` immediately because the target module can evolve or be removed independently.

Once complete, deletion is planned from the prospective pre-delete state before any delete effect is applied. A patch expression binds `.` to the referencing row as it existed at planning time and `$target` to the target row being removed. All direct and cascading targets are expanded to a fixed point; cascade cycles are valid and each row or set member is removed once.

A `restrict` ref blocks deletion only when its referencing row is outside that final delete set. Patches targeting a row that is itself deleted are ignored. Patches to a surviving row combine when they touch disjoint fields or assign the same resulting value; conflicting assignments reject the transition. The complete plan then applies atomically, updates refs and indexes, and checks every resulting constraint.

An ordinary collection deletion removes the target from live state. Its prior values remain available through retained history. A failing restriction, cascade, patch, check, or other state constraint rejects the entire transition.

### 21.2 Erasure

`erase(row)` is an explicit operation for removing live data and scrubbing retained payload bytes while preserving verifiable history structure.

Erasure removes exactly the reachable set an ordinary deletion of the same target would: the §21.1 delete-closure — the direct target plus every row a `cascade` policy pulls in to a fixed point, under the identical `restrict`/`none`/`= patch`/set-member effects. Its live-state scope is thus identical to deletion's. Erasure differs from deletion only in history: whereas deletion retains the prior values of removed rows in history (§21.1), erasure ALSO scrubs the retained history of every row in that closure — the right-to-be-forgotten — replacing each scrubbed occurrence with a digest stub. A cascade-deleted row is therefore scrubbed on the same footing as the direct target; a surviving row that is only patched keeps its history unscrubbed, exactly as under deletion.

Erasure is relocation, not destruction. Everything scrubbed — each scrubbed occurrence's payload, its row identity, and its retained history — is exported as a portable reintegration bundle (the extract), and capturing that bundle is a commit precondition. The operation is fail-closed: if the complete bundle cannot be durably captured, the erasure does not commit and no bytes are scrubbed, so scrubbed data is never made unrecoverable. A later load-action re-admits the bundle to reintegrate it (§21.3).

The operation atomically:

1. plans the same live removal and `$on_delete` effects as ordinary deletion, expanding the delete-closure to a fixed point;
2. captures, for every row in that closure, the retained payload required for possible reinsertion into the durable extract — the reintegration bundle carrying each occurrence's payload, identity, and retained history;
3. replaces each scrubbed occurrence in that closure with a digest stub representing the same logical leaf hash;
4. records the extract hash, occurrence map, and required attestations;
5. verifies the resulting retained history and artifact-entry checksums under the stub representation;
6. admits the erasure commit only once the bundle is captured, and yields the extract.

A failure in closure planning, extraction, bundle capture, durability, stubbing, hash verification, or attestation rejects the entire operation, leaving live state and retained history unchanged.

Authorization uses an explicitly exposed erasure call for the target. A delete grant does not silently become an erasure grant.

### 21.3 Reinsertion

`reinsert(extract)` verifies:

- extract content hash;
- required attestations;
- the referenced erasure history point;
- each requested occurrence's current digest stub.

It then restores requested bytes only where the exact expected stub remains. One mismatch rejects the complete reinsertion.

Reinsertion restores historical or live occurrences selected by the extract operation. Restoring historical bytes alone does not recreate a live row.

---

## Part V — Runtime and host contract

<a id="runtime"></a>

## 22. Runtime semantics

Runtime semantics define how package declarations become one coherent committed state and one coherent client-visible state. They constrain observable execution while leaving databases, clusters, schedulers, caches, and compilation strategies free to vary.

### 22.1 Guarantee classes

Liasse distinguishes constraints that hold in every state, conditions checked for one admission, and observations fixed by a commit. The distinction keeps historical facts stable while allowing future state and external conditions to change.

#### State constraints

State constraints hold in every committed state. They include:

- field and shape types;
- collection keys and additional uniqueness;
- reference validity and delete policy;
- field and row checks;
- structural module and interface compatibility.

#### Admission conditions

Admission conditions are established for the request being committed. They include:

- role membership and authenticator validity;
- mutation assertions;
- available meter capacity for new or changed spends;
- verified blob ingress;
- provider operations required by the request.

Their successful outcome becomes a fact of that commit. For example, removing a subscription later changes future capacity while preserving the funding recorded for earlier spends.

#### Recorded observations

Values obtained at admission and needed for stable committed behavior are recorded with the commit or operation metadata. They include generated identifiers, `now()`, provider results written into state, and funding allocations. Replay uses those recorded values.

### 22.2 Atomic admission

A mutation program is one proposed state transition. The engine evaluates its statements in order against a prospective state and admits the complete result once every applicable guarantee succeeds. A failure leaves the prior committed state intact.

A program producing no state change returns `unchanged` and creates no commit.

### 22.3 Concurrent and serial ordering

Every admitted request occupies one position in a serial execution order. The implementation defines one acyclic precedence relation through its declared admission mechanism. Ordering events MAY include causal completion, an assigned ingress sequence, comparable receive timestamps, a sequencer, database serialization, or consensus.

Once that mechanism establishes `A` before `B`, final admission MUST preserve `A < B`. Internal scheduling events outside the declared admission mechanism add no precedence. When the relation contains no path between two concurrent requests, either relative order is valid. The committed serial order is a linear extension of the established relation.

Applications requiring a specific outcome under otherwise unordered concurrency express it in state or assertions:

```hjson
"accept": [
  "assert(.version == @expected_version, 'Changed concurrently')"
  ".accepted = true"
  ".version = .version + 1"
]
```

### 22.4 Admission within mutation execution

The implementation evaluates requests in one serial order while preserving every precedence relation it has already established. It MAY evaluate speculatively, provided final admission revalidates the request at its actual position.

A request observes every committed request ordered before it. Two requests with no established precedence MAY appear in either order.

Examples:

```hjson
"rename": ".name = @name"
```

Two concurrent renames MAY both commit; the later serial position determines the resulting name.

```hjson
"withdraw": [
  "assert(.balance >= @amount, 'Insufficient funds')"
  ".balance = .balance - @amount"
]
```

Each withdrawal checks the balance at its own serial position. One MAY succeed and the next fail.

### 22.5 Time and recorded timestamps

A `timestamp` is a signed Unix-time count at a declared precision. `now()` follows transaction-timestamp semantics: one best-effort wall-clock instant is fixed for the complete external request, load, migration, or system transition, and every `now()` call in that operation returns that same instant converted to the target precision. Precision conversion rounds to the requested fractional-second precision.

A request that waits for locks or final serial admission keeps its fixed `now()` value, just as its other generated inputs remain fixed. Later commits MAY contain an earlier timestamp when the host wall clock moves backward; timestamps never establish commit order unless an implementation has separately declared a receive-time precedence relation. Commit order comes from admission order.

### 22.6 Temporal evaluation

The runtime evaluates current bucket activity from its best available wall clock at the package's declared precision. Wall-clock movement in either direction MAY cause rows to enter or leave active views; the runtime MUST reflect the resulting current logical view and emit a new live frontier in the order the temporal observation is established.

`now()` and row `$created` values used in admitted transitions are recorded once. Pinned calendar/time-zone rules ensure recurring bucket boundaries remain stable across nodes and replay.

### 22.7 Client coherence

A logical client connection MAY multiplex several explicitly selected authentication contexts and live views.

For a successful call, the runtime MUST:

1. durably commit the operation;
2. make it visible to reads at that commit or later;
3. re-evaluate authorization for every outgoing result on the connection;
4. advance every still-authorized subscription through the commit;
5. deliver the committed response.

A connection loss before admission MAY cancel the request. A loss during admission or response delivery leaves the result unknown to that client. The commit, when present, remains final. An optional operation identifier supports at-most-once execution and later status lookup.

### 22.8 External operations and actor provenance

Only authenticated external interface requests bind `$actor`. Public requests and engine maintenance run without an actor. Internal calls preserve the original bindings.

Reading `$actor` or `$session` where no actor is bound — a public request (§10.2), a host-operator transition (§23.5), or engine maintenance — is an error, never a `none` binding. A reference lexically inside a public surface's inline `$mut` program or inline `$view`, a context that can never bind an actor, is rejected at load (§10.2, §6.2). A reference reached indirectly — through a declared mutation that an authenticated role could also invoke, or through an operator or maintenance transition — keeps the declaration valid and faults at admission as an unbound structural read (§6.3), rejecting that one request. The host provenance recorded for an operator transition (§23.5) is not application-readable: no expression binding exposes it, and `$actor` never resolves to it.

Request inputs remain local to admission. A mutation makes selected values durable through explicit state writes. Generated and provider results enter durable state or retained history when required to reproduce committed state or another durable admission fact. Namespace-defined audit projections may record sanitized typed results suitable for diagnostics and replay.

---

<a id="rust-host"></a>

## 23. Rust host and implementation contract

The Rust host supplies resources, transports, namespaces, providers, connectors, and trusted operator access. A conforming runtime MAY use any physical architecture that preserves the package's declared values, identity, atomicity, ordering, history, authorization, and client-coherence guarantees.

### 23.1 Application responsibilities

A package defines:

- logical state shapes and configurable row identity;
- computed values and views;
- mutation programs and responses;
- state constraints and admission assertions;
- public surfaces, roles, authenticators, account/session mapping;
- bucket, meter, blob, keyring, history-retention, and module policies;
- required typed namespaces and provider capabilities.

These declarations determine observable results.

### 23.2 Host responsibilities

The Rust host supplies:

- package sources and registry access;
- namespace implementations;
- key providers and blob connectors;
- transport adapters and credential delivery;
- trusted operator capabilities;
- clock and time-rule data;
- surface mounts for directly exposed module APIs;
- resource budgets, storage, networking, and execution infrastructure.

The host retains its provider credentials and transport secrets under the contracts of the registered components.

### 23.3 Implementation freedom

An implementation MAY:

- use PostgreSQL, another database, or a custom store;
- flatten or preserve the logical tree;
- materialize or compute views and computed fields;
- precompile selectors, mutations, and surface requests;
- allocate separate resource pools or nodes by application, module, collection, or workload;
- shard, replicate, cache, batch, and incrementally maintain state;
- schedule temporal bucket transitions and maintenance eagerly or lazily;
- encode history and indexes in any internal form.

Every choice MUST preserve declared identity, values, order, atomicity, admission, history, live-view, and completion behavior.

### 23.4 Context construction

Representative integration:

```rust
let context = LiasseContext::builder()
    .namespace("cose", cose_namespace())
    .namespace("webauthn", webauthn_namespace())
    .key_provider("session-hsm", Arc::new(SessionHsm::new(config)?))
    .blob_connector("s3", Arc::new(S3Connector::new(config)?))
    .clock(Arc::new(SystemClock))
    .time_rules(TimeRules::pinned("2026a")?)
    .resources(ResourceBudget {
        memory_bytes: 8 * 1024 * 1024 * 1024,
        worker_threads: 16,
        ..Default::default()
    })
    .build()?;
```

The concrete Rust API MAY differ while exposing the same concepts and checks.

### 23.5 Trusted host access

The Rust host MAY hold explicit operator capabilities for administration, migration, recovery, or custom trusted operations. Host access bypasses external role authentication while retaining the package's type rules, module boundaries, refs, deletion planning, constraints, meters, provider contracts, serial admission, and atomicity.

An operator transition that changes the active application creates an ordinary commit with host provenance recorded separately from the application actor field.

---

### 23.6 Resource budgets

The host MAY allocate budgets to an application or subtree:

```text
memory
CPU/worker concurrency
query and mutation time
live subscription count
cache and materialization space
history and blob throughput
provider concurrency
network and storage quotas
```

Budget exhaustion produces operational backpressure or a rejected request according to the declared API contract. It never permits a partial state transition.

Resource placement is an implementation concern. A runtime MAY map modules or workloads to isolated processes, containers, or cluster nodes while preserving one logical application state and serial admission order.

### 23.7 PostgreSQL-backed implementation example

A PostgreSQL implementation MAY use:

- tables and indexed columns for keyed collections and refs;
- unique constraints for `$key` and `$unique`;
- transactions and row/predicate locking for serial admission;
- generated SQL or cached plans for views and mutations;
- ordered change records plus an outbox for live-view delivery;
- `ORDER BY` expressions matching Annex B, including explicit absence placement;
- materialized views or incremental tables for expensive computed data.

This is one conforming strategy. The normative contract is the logical behavior rather than the physical layout. A backend-internal encoding — such as a reversible NUL-safe escape for `jsonb` and `text` storage so U+0000 and every other `text` scalar survive (Annex A.1) — is exactly such an implementation-owned choice: it preserves the logical value and order without appearing at any logical surface.

### 23.8 Diagnostics

Diagnostics identify:

```text
phase       parse, type, load, auth, admission, provider, connector, migration
path        logical model/data path
surface     external target when applicable
operation   operation identifier when supplied
parameter   parameter name and declared type when applicable
expression  source expression and span when available
message     stable human-readable explanation
code        stable machine-readable category
audit       optional namespace-defined sanitized result
```

External argument values and credentials remain call-local. A package MAY expose application check messages. Runtime errors preserve their structured category independently of backend details.

---

### 23.9 Library operations

The principal Rust library operations are:

```text
create(artifact)
open(store)
load(target, artifact)
query / watch / call / operation_status
export / import / reconcile
branch / switch / fast_forward / rollback
modules.list / install / bind / update / enable / disable / uninstall
erase / reinsert
```

`create` establishes genesis from a `.liasse` artifact with fresh instance incarnations. `open` restores the active composition recorded by a store. `load` updates one package instance from an artifact definition. `export` produces the same recursive artifact type. Module `install` creates one module instance inside an existing module space.

---

## Part VI — Worked examples

> This part is informative. It demonstrates combinations of normative features; the examples do not add requirements.

---

## W1. Public task API

```hjson
{
  "$liasse": 1
  "$app": "example.tasks@1.0.0"

  "$model": {
    "tasks": {
      "$key": "id"
      "id": "uuid = uuid()"
      "title": {
        "$type": "text"
        "$normalize": "string.trim(.)"
        "$check": ["size(.) > 0", "A title is required"]
      }
      "done": "bool = false"
      "created_at": "timestamp = now()"

      "$mut": {
        "complete": [
          ".done = true"
          "return . { id, title, done, created_at }"
        ]
      }
    }

    "$mut": {
      "add_task": [
        "task = .tasks + { title: @title }"
        "return task { id, title, done, created_at }"
      ]
    }

    "open_tasks": {
      "$view": '''.tasks[:task | !task.done] {
        id,
        title,
        created_at,
        $sort: [-created_at]
      }'''
    }

    "$public": {
      "tasks": {
        "$view": ".open_tasks"
        "$mut": {
          "add": ".add_task"
          "complete": ".tasks[@id].complete()"
        }
      }
    }
  }
}
```

Calls:

```text
public.tasks.add { title: "Write docs" }
public.tasks.complete { id: <task-key> }
```

`add` infers `title: text`; `complete` infers `id: tasks.$key` from the selector.

---

## W2. Passkey login, direct tokens, and multiple sessions

This example requires external `webauthn` and `cose` namespaces and a key provider named `session-hsm`.

```hjson
{
  "$liasse": 1
  "$app": "example.accounts@1.0.0"

  "$requires": {
    "webauthn": "liasse.webauthn@1"
    "cose": "liasse.cose@1"
  }

  "$model": {
    "session_keys": {
      "$keyring": {
        "$provider": "session-hsm"
        "$algorithm": "Ed25519"
        "$rotate": "P30D"
        "$retain": "P45D"
      }
    }

    "accounts": {
      "$key": "id"
      "id": "uuid = uuid()"
      "name": "text"
      "enabled": "bool = true"
    }

    "logins": {
      "$key": ["kind", "issuer", "subject"]
      "kind": "text"
      "issuer": "text"
      "subject": "text"
    }

    "account_logins": {
      "$key": ["account", "login"]
      "account": { "$ref": "/accounts" }
      "login": { "$ref": "/logins" }
    }

    "sessions": {
      "$key": "id"
      "$bucket": ".expires_at"

      "id": "uuid = uuid()"
      "account": { "$ref": "/accounts" }
      "login": { "$ref": "/logins" }
      "device": "text?"
      "expires_at": "timestamp"
      "revoked": "bool = false"

      "$mut": {
        "revoke": [
          ".revoked = true"
          "return . { id, revoked }"
        ]
      }
    }

    "$mut": {
      "passkey_login": [
        "identity = webauthn.verify(@response)"
        "login = /logins[{ kind: 'passkey', issuer: identity.rp, subject: identity.credential }]"
        "mapping = /account_logins[{ account: @account, login: login.$key }]"

        '''session = /sessions + {
          account: mapping.account,
          login: mapping.login,
          device: @device,
          expires_at: now() + time.duration('P30D')
        }'''

        '''token = cose.sign(/session_keys, {
          auth: 'session',
          session: session.$key,
          expires_at: session.$until
        })'''

        "return { auth: 'session', token, expires_at: session.$until }"
      ]
    }

    "$auth": {
      "session": {
        "$credential": "bytes"
        "$verify": "cose.verify(/session_keys, $credential)"
        "$session": "/sessions[$proof.session]"
        "$actor": "/accounts[$session.account]"
        "$check": [
          "$proof.auth == $auth_name"
          "!$session.revoked"
          "$actor.enabled"
        ]
      }
    }

    "$public": {
      "login": {
        "$mut": { "passkey": ".passkey_login" }
      }
    }

    "$roles": {
      "account": {
        "$auth": "session"
        "$members": ".accounts[:a | a.enabled]"

        "profile": {
          "$view": "$actor { id, name }"
        }

        "sessions": {
          "$view": '''.sessions.$all[:s | s.account == $actor] {
            id, device, $from, $until, revoked
          }'''
          "$mut": {
            "revoke": ".sessions[@id].revoke()"
          }
        }
      }
    }
  }
}
```

Each login call creates a distinct session row and returns a distinct token. Every authenticated request names `role: account` and `auth: session`. One client can retain several tokens for different accounts or devices.

---

## W3. Overlapping heterogeneous subscription credits

```hjson
{
  "$liasse": 1
  "$app": "example.credits@1.0.0"

  "$model": {
    "plans": {
      "$key": "id"
      "id": "text"
      "credits": "decimal"
      "period": "period?"
      "price": "decimal"
    }

    "subscriptions": {
      "$key": "id"
      "id": "uuid = uuid()"
      "account": { "$ref": "/accounts" }
      "plan": { "$ref": "/plans" }
      "starts_at": "timestamp"
      "ends_at": "timestamp? = none"
    }

    "credit_periods": {
      "$bucket": {
        "$source": ".subscriptions"
        "$from": "$source.starts_at"
        "$until": "$source.ends_at"
        "$repeat": "/plans[$source.plan].period"
      }

      "credits": "= /plans[$source.plan].credits"
      "price": "= /plans[$source.plan].price"
    }

    "accounts": {
      "$key": "id"
      "id": "uuid = uuid()"
      "name": "text"

      "$limits": {
        "credits": {
          "$sources": {
            "subscription": '''/credit_periods[:p | p.$source.account == .] {
              $quantity: .credits,
              price
            }'''
          }
          "$order": ["$until", "price"]
        }
      }

      "spends": {
        "$key": "id"
        "$consumes": "credits"
        "id": "uuid = uuid()"
        "amount": "decimal"
        "occurred_at": "timestamp = now()"
      }

      "$mut": {
        "consume": [
          "spend = .spends + { amount: @amount }"
          "return spend { id, amount, occurred_at, funding }"
        ]
      }
    }
  }

  "$data": {
    "plans": {
      "weekly": { "credits": "30", "period": "P7D", "price": "9" }
      "monthly": {
        "credits": "100"
        "period": { "months": 1, "zone": "Europe/Paris", "overflow": "clamp" }
        "price": "20"
      }
      "quarterly": {
        "credits": "400"
        "period": { "months": 3, "zone": "UTC", "overflow": "clamp" }
        "price": "50"
      }
      "lifetime": { "credits": "1000", "period": "= none", "price": "200" }
    }
  }
}
```

At admission, a spend sees all subscription periods active at its `occurred_at`. Pools sort by `$until`, then price, then identity. Finite weekly/monthly/quarterly credits therefore drain before the lifetime pool whose `$until` is `none`.

---

## W4. Company-local template modules

### W4.1 Host fragment

```hjson
"companies": {
  "$key": "id"
  "id": "text"
  "name": "text"
  "plan": "text"
  "internal_notes": "text"

  "templates": {
    "$key": ["module", "template"]
    "module": "text"
    "template": "text"
    "label": "text"
    "journal": "text"
    "lines": "json"
  }

  "$mut": {
    "rename": ".name = @name"
    "import_template": [
      "source = .modules[@module]::templates[@template]"
      '''.templates + {
        module: @module,
        template: @template,
        label: source.label,
        journal: source.journal,
        lines: source.lines
      }'''
    ]
  }

  "modules": {
    "$modules": {
      "$expose": {
        "company": {
          "$view": ". { id, name, plan }"
          "$mut": { "rename": ".rename" }
        }
      }

      "$interfaces": {
        "templates": {
          "$view": {
            "$key": "id"
            "id": "text"
            "label": "text"
            "journal": "text"
            "lines": "json"
          }
        }
      }
    }
  }

  "available_templates": {
    "$view": '''.modules::templates {
      module: modules.$key,
      template: templates.$key,
      id,
      label,
      journal,
      lines,
      $sort: [label, module, template]
    }'''
  }
}
```

### W4.2 Module package

```hjson
{
  "$liasse": 1
  "$module": "acme.fr_sales_templates@1.0.0"

  "$use": {
    "company": "$parent"
  }

  "$model": {
    "templates": {
      "$key": "id"
      "id": "text"
      "label": "text"
      "journal": "text"
      "enabled": "bool = true"
      "lines": "json"
    }
  }

  "$data": {
    "templates": {
      "sale_invoice": {
        "label": "Facture de vente"
        "journal": "VE"
        "enabled": "= #company.plan == 'fr-pcg'"
        "lines": [
          { "account": "411", "side": "debit", "amount": "'= total_ttc" }
          { "account": "707", "side": "credit", "amount": "'= total_ht" }
        ]
      }
    }
  }

  "$expose": {
    "templates": {
      "$view": ".templates[:t | t.enabled] { id, label, journal, lines }"
    }
  }
}
```

Installing the package below another company binds `#company` to that company's projected parent surface and creates a separate private template table.

---

## Part VII — Normative annexes

---

## Annex A — Types and canonical wire values

This annex is normative.

### A.1 Primitive types

| Type | Meaning | Canonical strict-JSON value |
|---|---|---|
| `text` | Unicode scalar sequence | JSON string, preserved exactly |
| `bool` | Boolean | `true` or `false` |
| `int` | Arbitrary-precision integer | JSON string with canonical base-10 digits |
| `decimal` | Exact decimal | JSON string, no exponent, minimal-scale canonical form (defined below) |
| `bytes` | Small binary value | `{ "$bytes": "<canonical base64>" }` |
| `uuid` | 128-bit UUID | lowercase hyphenated JSON string |
| `date` | Gregorian calendar date | `YYYY-MM-DD` JSON string |
| `timestamp` | signed Unix-time count at declared precision | canonical base-10 JSON string |
| `duration` | exact elapsed duration | canonical ISO-8601 duration string |
| `period` | fixed or calendar recurrence step | form defined in A.4 |
| `json` | canonical JSON value | JSON null/bool/number/string/array/object |
| `blob` | binary-content descriptor | descriptor object defined in [Blobs](#blobs) |
| `enum` | one declared label | JSON string |
| `none` | absence of an `optional<T>` value | represented by position — see below; no wire sentinel |

JSON `null` is a value of `json`. `none` is absence in the Liasse type system, not a value: it cannot be a member of a set, a map value, or a distinct thing carried by a wire marker. `none` is therefore represented by *position*, never by a sentinel:

- **optional object member** (struct field, singleton member, seeded row field): `none` is the member **omitted** from the wire object; a present member is a present value.
- **set element**: `none` is **not a member**. `none` is never a valid set element, and adding `none` to a set is a no-op that yields the same set.
- **map value**: `none` is the **key absent**. A map never stores a `none` value; absence is the key not being present.
- **fixed-arity positional composite element** (a positional slot that cannot be omitted): `none` is JSON **`null`** in that position. `null` is unambiguous there because it is not the canonical wire form of any scalar type. For a positional `optional<json>` slot specifically, a positional `null` is `none`; a *present* JSON `null` cannot be written positionally and MUST be object- or array-wrapped.
- **storage**: `none` is the backend's native NULL.

There is no `{ "$none": true }` sentinel; it is not produced and carries no `none` meaning on input — a `json` object whose literal shape is `{ "$none": true }` is an ordinary present value that round-trips as itself.

**Canonical input is mandatory at the machine wire/request boundary.** A scalar value crossing the machine wire/request boundary (a request argument, a `view`/`call` parameter, any value an untrusted peer supplies) MUST already be in its canonical Annex-A / D.2 form. A non-canonical spelling — an uppercase `uuid`, a leading-zero, `+`-signed, or `-0` `int`, a non-canonical `base64` padding or variant, a non-canonical `duration` or `timestamp` spelling — is **rejected as malformed at admission**; it is never normalized and never accepted as a distinct value. This keeps one wire spelling per value, so a non-canonical spelling can neither mint a second identity nor alias an existing one. The human-authoring layer (Annex C package definitions and authored `$data`) is exempt: it stays lenient and is canonicalized at compile.

`text` is a sequence of Unicode scalar values (U+0000..U+10FFFF, excluding surrogate code points), preserved exactly. No scalar value is excluded; in particular U+0000 (NUL) is a legal `text` scalar. A backend whose native string or document storage cannot represent a given scalar value MUST apply a reversible, backend-internal encoding that preserves the value losslessly and does not alter any observable result — value equality, Annex B order, row keys, or opaque identity tokens. Per §23.3 that encoding is implementation-owned and MUST NOT appear at any logical surface.

A `decimal`'s canonical wire value is its **minimal-scale plain form**: no exponent; every trailing fractional zero removed, and the decimal point omitted when no fractional digit remains; a single leading `-` for a negative value; zero is `0` and negative zero is never produced. The canonical string is therefore a total function of the decimal's mathematical value, so numerically equal decimals (A.6, B.1) share exactly one canonical spelling — scale is not part of a decimal's identity. (Trailing zeros in the *integer* part are magnitude, not scale, and are preserved: `100` stays `100`; a quotient such as `10 / 4` is `2.5`, and `1.50 − 0.50` is `1`.)

### A.2 Type expressions

```text
text | bool | int | decimal | bytes | uuid | date | timestamp
| duration | period | json | blob

named_type
T?                              shorthand for optional<T>
optional<T>
set<T>
map<K, V>
view<T>
ref<target>
{ field: T, optional_field?: U }
collection.$key
/absolute.collection.$key
#surface.$key
```

Keyed collections represent application row sequences through explicit `$sort`. Sets represent unique membership. `json` carries schema-free JSON values, including JSON arrays, as one typed value.

### A.3 Field declarations

| Form | Meaning |
|---|---|
| `"name": "text"` | required text field |
| `"name": "text?"` | optional text field |
| `"enabled": "bool = true"` | writable boolean with insertion default |
| `"total": "= .net + .tax"` | computed read-only value |
| `"role": { "$enum": [...] }` | enum field |
| `"owner": { "$ref": "/accounts", ... }` | checked ref |
| `"tags": { "$set": "text" }` | state set |
| plain object | static struct |
| object with `$key` | keyed collection |
| object with `$view` | computed view |
| object with `$modules` | module space |
| object with `$keyring` | keyring |

Expanded field keys include:

```text
$type
$optional
$default
$normalize
$check
$unique
$precision          timestamp precision override
$from / $as / $back migration mapping
```

### A.4 Period values

A fixed period uses an ISO-8601 duration containing only elapsed day/time components:

```hjson
"P7D"
"PT15M"
```

A calendar period is an object:

```hjson
{
  "years": 0
  "months": 1
  "weeks": 0
  "days": 0
  "time": "PT0S"
  "zone": "Europe/Paris"
  "overflow": "clamp"
  "ambiguous": "earlier"
  "missing": "forward"
}
```

Omitted fields use the shown zero/default values. At least one magnitude component MUST be non-zero.

Boundary `i` is calculated from the original series anchor using `i × period`, rather than repeatedly adding to the previous clipped boundary. This preserves end-of-month anchors:

```text
January 31, monthly, clamp -> January 31; February 28/29; March 31; ...
```

Policies:

| Field | Values | Meaning |
|---|---|---|
| `overflow` | `clamp`, `reject` | destination calendar date missing |
| `ambiguous` | `earlier`, `later`, `reject` | local time occurs twice |
| `missing` | `forward`, `backward`, `reject` | local time does not occur |

Time-zone rule data is pinned by the package load environment and recorded in the application installation metadata.

### A.5 Timestamp precision

```text
s    seconds
ms   milliseconds
us   microseconds
ns   nanoseconds
```

The package default is `us`, matching PostgreSQL timestamp precision. A timestamp value is a signed count since `1970-01-01T00:00:00Z`. Arithmetic converts operands to a common exact precision. A value exceeding the declared range produces a diagnostic.

`now()` follows transaction-start semantics: one host wall-clock sample is fixed for the complete admitted operation and every call observes that same instant. Precision conversion follows the PostgreSQL timestamp precision rule. The represented precision is an application contract; clock accuracy remains best effort. This shared-instant rule is specific to `now()`, whose value means the instant the transaction ran; `uuid()` is the symmetric opposite — a fresh, distinct value on every evaluation, so one field-default call site across several rows yields a distinct value per row and two rows never share a generated `uuid()` (§5.1, §8.12).

### A.6 Decimal semantics

Decimals are exact base-10 values. Addition, subtraction, and multiplication are exact. Integer division truncates toward zero. `avg` converts its inputs to `decimal` and returns `decimal?`.

The remainder operator `%` is part of the arithmetic surface for `int` and `decimal`. It is defined as `a − trunc(a ÷ b) × b`, where the quotient truncates toward zero; the remainder therefore takes the sign of the dividend and satisfies `(a ÷ b)·b + (a % b) = a`, matching PostgreSQL `mod` (`-7 % 2 = -1`, `7 % -2 = 1`). A zero divisor in `/` or `%` produces no value: it is a typed evaluation error that rejects the containing evaluation — computed field, check, or mutation — with a diagnostic, and never panics, yields `none`, or yields a non-finite value. Because a divisor MAY be read from state, this is detected at evaluation rather than at load; a short-circuiting operator (`&&`, `||`, `?:`, `??`) never evaluates an unreached divisor. Arithmetic and remainder operators require present (non-optional) numeric operands (`int` or `decimal`; `+` additionally concatenates two `text`). An `optional<T>` operand is a static type error at load — coalesce it (`x ?? 0`) or narrow it first. Operators do not skip, propagate, or zero-fill `none`, in contrast to the explicit aggregate rule (§7.5) and ordering rule (Annex B.2).

The default decimal division and rounding semantics follow PostgreSQL `numeric`:

- a non-terminating quotient is computed to an internal rounding precision of at least sixteen significant fractional digits; the emitted value is the exact result rendered in A.1's minimal-scale canonical form, so a terminating quotient such as `10 / 4` is `2.5` (its significant digits are never zero-padded, and numerically equal results share one spelling). The operand display scale does not floor the emitted spelling — minimal-scale rendering subsumes it;
- that internal rounding precision is bounded by the implementation limits defined for the Liasse language version — the same scale bound A.7 applies to a `json` number;
- rounding at a selected decimal scale resolves a halfway value away from zero.

A package MAY select another standard division scale or rounding mode through `$semantics`. Supported explicit rounding values are:

```text
half_even
half_away_from_zero
toward_zero
away_from_zero
floor
ceiling
```

A field MAY add scale or range checks without changing package arithmetic.

### A.7 Canonical JSON

Canonical JSON:

- preserves JSON `null`;
- sorts object keys by the Liasse text order;
- emits numbers in the canonical JSON-number spelling (plain, no exponent, minimal scale — the same spelling as an A.1 `decimal`);
- bounds a number's scale magnitude (its base-ten exponent — its fractional-digit or trailing-zero count) by the same implementation scale limit as an A.6 `decimal`. A `json` value containing a number whose scale magnitude exceeds that limit is rejected at the decoding boundary with the same diagnostic class as an out-of-range `decimal`; a conforming implementation MUST NOT accept it or attempt to canonicalize it;
- preserves array order;
- contains no `none` value.

A Liasse optional JSON value has type `json?`, allowing both `null` and `none`.

### A.8 Key-eligible types

Collection key fields MAY use:

```text
text, bool, int, decimal, bytes, uuid, date, timestamp, duration, enum,
and structs composed solely of key-eligible required fields
```

Optional values, JSON, blobs, sets, maps, and views are excluded from row keys. Composite collection keys combine several eligible fields. Candidate-key components use the same eligible base types, although the candidate-key fields themselves MAY be optional; rows containing `none` in any candidate-key component do not participate in that constraint.

Every scalar key component MUST have non-empty canonical key text (D.2). A key value that flattens to an empty component — including an empty `text` value or an empty `bytes` value (whose base64 is the empty string) — is not admissible: commit admission rejects it in the same failure class as an unpopulated required key field (§22.1). Consequently every key segment of a display path (D.3) is non-empty, so display paths stay injective and round-trippable, and a display-path parser MUST reject a path containing an empty segment.

### A.9 Refs

`ref<T>` has the exact key type of its target collection or keyed view. A scalar key uses its scalar wire value. A composite key uses an array of component wire values in `$key` order; named object selectors are authoring syntax for the same typed tuple. Target path information comes from the field declaration.

---

<a id="annex-b"></a>

---

## Annex B — Deterministic total order

This annex is normative.

Every sortable Liasse value has a deterministic ascending total order. Descending order reverses it. Sort keys compare lexicographically from left to right; row identity and then occurrence identity complete the order where applicable.

### B.1 Scalar order

| Type | Ascending order |
|---|---|
| `bool` | `false`, then `true` |
| `int` | mathematical integer order |
| `decimal` | mathematical decimal order; numerically equal canonical values compare equal — one minimal-scale canonical spelling per value (A.1) |
| `text` | lexicographic Unicode scalar-value order |
| `bytes` | lexicographic unsigned-byte order |
| `uuid` | unsigned lexicographic order of its 16 bytes |
| `date` | chronological Gregorian order |
| `timestamp` | signed Unix-time order after exact precision normalization |
| `duration` | exact elapsed-duration order |
| `enum` | declaration order |
| `period` | fixed periods before calendar periods; fixed by exact duration; calendar by `(years, months, weeks, days, time, zone, overflow, ambiguous, missing)` |
| `ref<T>` | target key order |

Calendar policy labels use the declaration order shown in Annex A:

```text
overflow:  clamp < reject
ambiguous: earlier < later < reject
missing:   forward < backward < reject
```

Applications needing case folding, locale collation, natural-number text sorting, or domain priority produce an explicit sort key:

```hjson
"$sort": ["string.casefold(name)", "name"]
```

### B.2 Optional values and `none`

The ascending order of `optional<T>` is:

```text
all present T values in ascending order, then none
```

Descending reverses the complete order:

```text
none, then all present T values in descending order
```

This matches PostgreSQL's default absence placement.

Examples:

```text
ASC  timestamp?    earliest ... latest, none
DESC timestamp?    none, latest ... earliest
```

### B.3 JSON order

JSON values use this type rank:

```text
null < bool < number < string < array < object
```

Within each rank:

| JSON kind | Order |
|---|---|
| `null` | one value |
| boolean | `false < true` |
| number | mathematical numeric order |
| string | Liasse text order |
| array | lexicographic element order; shorter array first after a shared prefix |
| object | keys sorted by text order, then lexicographic `(key, value)` pair order |

For `json?`:

```text
ASC    null ... object, none
DESC   none, object ... null
```

### B.4 Composite values

| Type | Order |
|---|---|
| struct | lexicographic fields in canonical field-name order |
| composite key | lexicographic components in `$key` order |
| set | sort members by element order, then compare the resulting member sequences |
| map | sort entries by key order, then compare key/value pairs |
| blob descriptor | `$sha512`, `$bytes`, `$media`, then optional `$name` |

Within this structural order an **absent optional member sorts last** among values equal on every preceding member — a present value precedes an absent one, consistent with B.2's present-before-`none`. So of two blob descriptors equal on `$sha512`, `$bytes`, and `$media`, the one carrying a `$name` sorts before the one that omits it; likewise a calendar period naming a `zone` (B.1) sorts before an otherwise-equal one that omits it, and a struct or composite value whose optional member is `none` sorts after one whose corresponding member is present.

Blob content identity uses `$sha512`; descriptor ordering and equality include the complete descriptor.

### B.5 Rows and views

A collection defaults to key ascending. A view follows its declared `$sort`, then inherited or synthetic row identity, then occurrence identity.

```hjson
"$sort": ["-$until", "price"]
```

means:

1. `none` upper bounds first;
2. finite upper bounds latest to earliest;
3. price ascending among equal upper bounds;
4. occurrence identity ascending as the final tiebreaker.

A sort is therefore total even when every declared sort expression is equal or the same row occurs repeatedly.

### B.6 Meter order

Meter `$order` uses exactly the same value order. It adds pool incarnation as the final tiebreaker. `$until` receives no special case beyond its type `timestamp?`.

---

<a id="annex-c"></a>

---

## Annex C — Compact grammar and syntax index

This annex is normative as a syntax index; detailed semantics remain in the feature chapters.

### C.1 Package

```text
app-package := {
  $liasse, $app,
  $semantics?, $requires?, $resources?, $types?, $model, $data?, $history?, $migrations?
}

module-package := {
  $liasse, $module,
  $semantics?, $requires?, $resources?, $types?, $config?, $model, $data?,
  $history?, $use?, $deps?, $expose?, $migrations?
}
```

Exactly one of `$app` and `$module` identifies a package. `$liasse` is the required supported language-generation integer.

```text
$resources: {
  logical_name: {
    $path: relative-archive-path
    $media: media-type
    $sha256: lowercase-hex-digest
  }
}

$history: all | { $minimum: duration }
```

Each resource name identifies one verified archive entry. Object member order carries no package semantics.

### C.2 Shape markers

```text
$key        keyed collection
$set        unique set
$view       computed view
$modules    module space
$keyring    managed keyring
$bucket     lifecycle/period collection behavior
$limits     meter declaration
$consumes   spend declaration
$mut        named mutation map
$roles      authenticated external APIs
$public     unauthenticated external APIs
$auth       authenticator declarations
$history    minimum recoverable-history policy
$blob_storage blob placement policy
```

A plain object without a shape marker is a static struct. An object's node kind is fixed by exactly one kind marker among `$key`, `$set`, `$view`, `$ref`, `$enum`, `$type`, `$keyring`, `$modules`, and `$like`; `$bucket` composes with `$key`. An object bearing two mutually-exclusive kind markers is a static error that names both (§5.3).

### C.3 Field forms

```text
"field": "T"
"field": "T?"
"field": "T = default_expression"
"field": "= computed_expression"
"field": { $type: T, ... }
"field": { $enum: [...] }
"field": { $ref: target, $on_delete?: restrict | cascade | none | "= patch" }
"field": { $set: T }

$key: field | [field, ...]
$unique: [field | [field, ...], ...]
$check: expression | [expression, message] | [[expression, message], ...]
```

A `$check` on an expanded field checks that field; on a struct or collection shape it checks the complete prospective struct or row.

### C.4 Expression positions

Always-expression positions:

```text
$view, $check, $normalize, $as, $back, $members,
$verify, $actor, $session, $eligible, $order expressions,
selector filters, projections, mutation statements
```

Literal-or-expression positions:

```text
$data values and expanded $default
```

Default expressions, including the `T = default_expression` shorthand, use the full value and view expression surface and may read any logical state visible from their declaration scope.

In literal-or-expression positions:

```text
"= expr"      expression (expr MUST be non-empty)
"'= text"     literal text beginning with =
"'text"       literal text with one leading ' removed
```

The bare marker `"="` — the expression form with an empty body — is a static error, neither a literal `=` nor an empty result; write `"'="` for the literal (§4.2).

### C.5 Roots and names

```text
/             package root
.             current value
^, ^^         lexical parents
#name         imported surface/module binding
@name         parameter
name          local/row binding
$name         structural binding defined by its feature context
$config       current module installation configuration
$old          source state during a migration
$target       deletion target during an `$on_delete` patch
none          optional absence
```

### C.6 Selectors

```text
rows[key]
rows[key_a, key_b, key_collection]
rows[:binding]
rows[:binding | condition]
rows::                         same-name row binding
```

Every selector yields a row view. A scalar key yields zero or one occurrence; key collections preserve input order and repetitions. Contexts requiring one row reject zero or multiple occurrences. A wildcard selection syntax is absent; projections name fields explicitly.

### C.7 Projection

```text
source {
  field
  field: expression
  @parameter
  binding.field
  binding:
  nested: { ... }
  $key: field_or_fields
  $sort: [...]
  $skip: integer
  $limit: integer
}
```

### C.8 View combinators

```text
a | b
a & b
a - b
( a )                grouping
condition ? a : b
a ?? b
[]
```

`|` and `&` share one precedence level: a chain repeating one combinator is left-associative, but a chain mixing `|` and `&` MUST be parenthesized, otherwise it is a static error. Grouping uses `( )` (§6.1). Difference (`-`) binds at the arithmetic-subtraction level, tighter than `|` and `&`; `??` and the `? :` conditional occupy their own levels (§7.4).

### C.9 Mutation programs

```text
$mut: {
  name: statement
  name: [statement, statement, ...]
  "name({ explicit: Type })": ...
}
```

A program is one statement (a bare string) or an ordered array of statements. The array MUST contain at least one statement; the empty array `[]` is a static error (§8.1).

Statements:

```text
local = value_or_mutation_result
collection + view
collection = view
row_source { patch }
collection - keys
-row_source
field = value
field -
set + values
set - values
mutation()                              no-parameter call; equivalent to mutation({})
mutation({ named_args })
assert(condition, message)
return value_or_view                  final statement
```

### C.10 Surface

```text
surface := {
  $params?: shape,
  $view?: view-expression,
  $mut?: {
    external_name: declared-mutation-reference | mutation-program
  }
}
```

A declared row-mutation reference includes a receiver expression resolving exactly one row before the mutation name. Parameters used by that receiver are combined with the referenced mutation's parameters. A collection without row selection is not a valid row-mutation receiver. An explicit call expression may map the surface parameters to different mutation arguments.

### C.11 Role and public API

```text
$public: {
  surface_name: surface
}

$recursive: {
  $field, $through, $bind, $where?, $except?
}

$roles: {
  role_name: {
    $auth: auth_name | [auth_name, ...]
    $members: view-expression
    surface_name: surface
  }
}
```

### C.12 Authenticator

```text
$auth: {
  name: authenticator | $parent.exposed_auth
}

authenticator := {
  $credential: Type
  $verify: expression
  $session?: exact-one-row-expression
  $actor: exact-one-row-expression
  $check?: expression | [expression, ...]
}
```

Every authenticated role lists its accepted names. Every authenticated request explicitly selects one accepted name.

### C.13 Bucket

```text
$bucket: until-expression

$bucket: {
  $source?: view-expression
  $from?: timestamp-expression
  $until?: optional-timestamp-expression
  $repeat?: optional-period-expression
}
```

Structural bindings:

```text
$created, $source, $from, $until, $index
```

### C.14 Meter

```text
$limits: {
  meter_name: {
    $sources: { source_name: pool-view }
    $eligible?: bool-expression
    $order?: [sort-expression, ...]
  }
}

$consumes: meter_name
$consumes: { meter_name: amount-expression | consume-config }

consume-config := {
  $amount?: numeric-expression
  $time?: timestamp-expression
  metadata_name?: expression
}
```

Pool structural fields:

```text
$quantity, $from, $until
```

Spend structural fields:

```text
$amount, $time
```

### C.15 Modules

```text
module-package.$config?: shape

$modules: {
  $expose?: { name: surface }
  $interfaces?: {
    name: {
      $view?: shape
      $mut?: { mutation-contract-name: mutation-contract }
    }
  }
  $auth?: { child_name: parent_auth_name }
}

$use: {
  handle: $parent | $parent.surface | peer-spec
  $optional?: { handle: peer-spec }
}

$deps: { handle: package-spec }
$expose: {
  interface-name: {
    $view?: view-expression
    $mut?: { contract-name: declared-mutation-reference }
  }
}
$if_module: optional-use-handle

$migrations: {
  exact-source-package-version: [migration-statement, ...]
}
```

`$if_module` appears inside the declaration it guards and depends only on whether the named optional module binding is present and enabled. Ordinary `has(#handle)` expressions affect values, while structural presence follows `$if_module`. Parent and peer access resolves only through an interface member bound under `$expose`.

### C.16 Keyring

```text
$keyring: {
  $provider: name
  $algorithm: name
  $usage?: [name, ...]
  $rotate?: duration | { $every, $overlap?, $mode? }
  $retain?: duration
  $protection?: name
}
```

### C.17 Blob storage

```text
$blob_storage: {
  $in: placement
  $serve?: store-view
}

placement := store-view
           | { $all: [placement, ...] }
           | { $any: [placement, ...] }
           | { $copies: int, $of: store-view }
```

---

<a id="annex-d"></a>

---

## Annex D — Canonical identity, paths, and integrity

This annex is normative for portable artifacts and verification.

### D.1 Row and instance identity

A collection row has an application address formed from its package instance, ancestor collection keys, and local `$key`. It also has an opaque immutable incarnation allocated at insertion. Rekeying preserves the incarnation; deletion followed by insertion allocates a new one.

A ref's visible wire value is the target's current typed key. Its durable relationship binds the declared target relation and target incarnation. Historical actor, session, meter-pool, deletion, and module coordinates include incarnation identity wherever key reuse could otherwise conflate occurrences.

A package instance has its own immutable incarnation. Renaming or rebinding an existing instance preserves it; uninstall followed by installation creates a new one.

A view row inherits source incarnations unless its projection declares synthetic identity. Each ordered occurrence additionally has an occurrence identity derived from the view declaration and multiplicity-producing source occurrences. A source-backed bucket row combines its source incarnation with `$from` for repeated periods unless it declares a custom `$key`.

### D.2 Canonical scalar key text

Each scalar key component has canonical text:

```text
text        exact Unicode text
bool        true | false
int         canonical decimal digits
decimal     canonical decimal text
bytes       canonical padded base64
uuid        lowercase hyphenated form
date        YYYY-MM-DD
timestamp   signed Unix count in declared precision
duration    canonical ISO-8601 text
enum        label
struct      its components in canonical field-name order
```

Within a scalar key component, each original `%`, `/`, and `:` is encoded respectively as `%25`, `%2F`, and `%3A`; the percent signs introduced by these escape sequences are not encoded again. This encoding is applied before components are joined. Composite components are joined by `:` in `$key` order, while canonical structured wire values use an array in the same order. The key type makes decoding unambiguous.

This encoding is used for seed object member names, display paths, and canonical textual exports. Expressions use typed key values rather than encoded strings. A `decimal` key component uses the minimal-scale canonical decimal text (A.1), so numerically equal decimal keys share one key text and one identity. Every scalar key component's canonical text is non-empty: a key value flattening to an empty component (an empty `text` or `bytes`) is inadmissible (A.8), so no display path (D.3) has an empty segment.

### D.3 Paths

A display path alternates declaration-name segments and canonical local key-text segments:

```text
/companies/acme/offices/paris/rooms/main
```

Every declaration-name path segment encodes each original `%` and `/` as `%25` and `%2F`. Key segments use the scalar-component encoding above before composite joining. Escape prefixes introduced by either process are not encoded again. Display paths identify logical rows within one loaded package tree. Refs store typed keys plus their declared target, not path strings.

A §8.2 root-singleton member is addressed by a name-only path — a bare declaration-name segment with no key segment (`/flag`), since D.1 gives a root member no ancestor collection key. Any internal reserved storage row used to hold singleton state is not part of the address space; its name and its placeholder key never appear in a display path (an empty key segment is not a well-formed display path).

A declaration MAY carry an optional stable `$id` when migration or tooling MUST preserve declaration identity across a rename or move. The common case uses the declaration's logical package path.

### D.4 Canonical definition identity

The definition identifier is SHA-256 over the canonical bytes of `liasse.json`:

- object member names sorted by Unicode scalar order;
- canonical primitive wire values;
- exact package version, requirements, resource descriptors, and resource digests included;
- authoring comments, whitespace, and Hjson conveniences removed.

The identifier covers the inert definition and declared resources. ZIP entry ordering, compression, timestamps, selected state, instance identity, and history do not participate. The loaded model has an implementation-owned runtime representation and no canonical identity.

### D.5 Portable `.liasse` encoding and integrity

A portable `.liasse` artifact uses ZIP64 and the entry structure defined in Section 19.5. `manifest.json`, `liasse.json`, and `history/index.json` use strict canonical JSON. State snapshots, deltas, checkpoints, and history archives use deterministic CBOR and are compressed as independent Zstandard frames.

Canonical CBOR follows these rules:

- integers use the shortest valid representation;
- maps use canonical encoded-key order;
- text uses exact Unicode scalar sequences;
- decimals use the canonical Liasse decimal wire value;
- timestamps use signed Unix counts together with declared precision;
- `none` and JSON `null` use distinct typed representations;
- rows, refs, declarations, and module instances include their stable typed identities where required.

`state/current.cbor.zst` decodes to:

```hjson
{
  "format": 1
  "instance": "<instance-incarnation>"
  "selected": { "lineage": "<lineage-id>", "point": "<point-id>" }
  "definition": "sha256:..."
  "configuration": <typed-value>
  "bindings": <typed-value>
  "state": <typed-value>
  "modules": {
    "<mount-name>": {
      "instance": "<child-incarnation>"
      "lineage": "<lineage-id>"
      "point": "<point-id>"
    }
  }
}
```

A history segment decodes to:

```hjson
{
  "format": 1
  "lineage": "<lineage-id>"
  "base": "<point-id>"
  "tip": "<point-id>"
  "points": {
    "<point-id>": {
      "previous": "<point-id>"
      "transaction": "<transaction-id>"
      "delta": <delta>
    }
  }
  "forward": <delta>
  "reverse": <delta>
  "archive": "history/archives/<range-id>.cbor.zst"
}
```

`previous`, `transaction`, `delta`, and `archive` are omitted when they do not apply. `points` identifies every retained point covered by the segment. A detailed segment carries each point delta. A compacted segment carries the combined `forward` and `reverse` deltas and an archive containing the omitted point details.

A delta is a canonical map from logical coordinate to:

```hjson
{
  "before": { "absent": true } | { "present": <typed-value> }
  "after":  { "absent": true } | { "present": <typed-value> }
}
```

A logical coordinate identifies the owning stable declaration, row incarnation when present, stored field or structural member, and typed set or map member when present. An archive decodes to `{ "format": 1, "replaced": { "<entry-name>": <entry-bytes> } }`; expanding it restores the exact replaced portable records.

`manifest.json` records a SHA-256 checksum and uncompressed media type for every required non-manifest entry. A nested module artifact is covered by the checksum of its exact `.liasse` bytes. Verification succeeds only when every referenced entry exists exactly once and every checksum matches.

History points and ancestry use explicit identifiers and lineage metadata. Checksums protect represented artifact bytes. Runtime transaction ordering, state identity, and ancestry follow their dedicated Liasse semantics.

### D.6 Recorded observations

A recorded observation contains:

```text
stable path/name within the operation
typed canonical value
provider or namespace descriptor identity when applicable
```

Generated values and provider outputs are included in retained portable history when they are required to reproduce admitted state or another durable admission fact.

### D.7 Erasure stubs

A scrubbed retained value is represented as:

```hjson
{
  "$stub": "sha256:<digest of original canonical typed value>"
  "$type": "<canonical Liasse type identity>"
  "$bytes": "<canonical payload byte length>"
}
```

The erasure extract maps each affected instance, history point, and logical coordinate to the original canonical bytes and expected stub. Reinsertion restores bytes only when the current stub matches. Artifact manifests and history archives are rechecksummed after erasure or reinsertion.

### D.8 Operation identity

An external operation identifier is scoped to:

```text
application instance
+ public or scoped-role target
+ explicitly selected authenticator when present
+ client-supplied high-entropy identifier
```

The runtime stores a request digest and final status for its configured operation-record lifetime. An equivalent retry in the same scope returns the retained status and never executes the mutation twice. Another digest rejects as an operation-identity conflict. The retained status contains the runtime commit position and client frontier or diagnostics; mutation return values remain ephemeral. No exported history point is required at response time.

---

<a id="annex-e"></a>

---

## Annex E — Package compatibility

This annex is normative.

### E.1 Version rule

Package identifiers use semantic versions.

```text
major    may change or remove boundary contracts
minor    may add or widen compatible boundary contracts
patch    preserves the same boundary contracts while correcting their implementation
```

A registry rejects a minor or patch release whose effective boundary contract narrows relative to an earlier release in the same major. `load` and module update apply the same check before activation.

Every shorthand and omission expands according to the package's fixed `$liasse` language version. A later package release cannot reinterpret an earlier omission. External authenticated requests always name their authenticator, resolved module bindings remain pinned until an explicit rebind, and inferred parameter and response shapes are compared after inference.

### E.2 Boundary contracts

Compatibility is defined at boundaries used by independently versioned clients or packages:

- public and authenticated surface addresses;
- view parameter, output-shape, identity, and explicit ordering contracts;
- mutation parameter, response, and receiver contracts;
- accepted authenticator names and credential or proof shapes;
- module-space `$interfaces` and the view and mutation contracts bound through `$expose`;
- parent, peer, and private dependency requirements;
- blob, namespace, provider, and keyring capabilities required to use those contracts;
- migrations required to activate the release over an existing instance;
- behavioral guarantees explicitly declared by those contracts.

A boundary contract is the complete promise another component may rely on. Private collections, helper mutations, implementation expressions, algorithms, storage layout, materialization, and internal migrations remain implementation details while the declared contracts continue to hold.

Compatibility therefore follows substitutability: every interaction valid under the earlier contract remains valid, and every result promised by that contract remains a valid result of the new release. Behavior outside the declared contract carries no compatibility promise.

### E.3 Mechanical and semantic checking

The checker compares canonical effective contracts after shorthand expansion, type inference, and module-interface binding. It MUST reject every narrowing it can establish from those contracts.

Mechanically decidable checks include:

- names, addresses, and selected interface majors;
- required and optional parameters;
- types, optionality, defaults, parameter normalization, and parameter checks;
- response and view shapes;
- row identity and explicit ordering;
- accepted authenticator names;
- required host capabilities;
- presence and shape of bound module views and mutations.

Arbitrary expression equivalence is not generally decidable. A package author MUST use a new major when a boundary-visible semantic change falls outside the mechanically compared structure and ceases to satisfy an earlier declared guarantee. Registries MAY require an explicit compatibility attestation for such releases.

Private expression changes are never treated as narrowing solely because their source differs. Their compatibility is determined by the boundary contracts they implement.

### E.4 Input compatibility

A compatible release accepts every request or binding accepted by the earlier boundary contract.

Compatible changes include:

- adding an optional parameter with a default;
- widening an explicitly declared numeric range or accepted enum domain;
- adding another explicitly selectable authenticator while retaining every existing name;
- adding a new surface, interface, or mutation name;
- rebinding an interface mutation to a different private implementation that satisfies the same contract.

Breaking changes include:

- adding a required parameter;
- narrowing a parameter check or accepted enum domain declared by the boundary;
- removing an accepted authenticator;
- changing the receiver identity expected by an exposed row mutation;
- requiring a stronger blob, namespace, provider, or keyring capability from an existing contract;
- removing a previously accepted module-interface binding.

Checks and assertions internal to a mutation remain private unless the boundary contract exposes their precondition or error guarantee. Once declared at the boundary, tightening them is narrowing.

### E.5 Output compatibility

A compatible release preserves every output property promised by the earlier boundary contract.

Compatible changes include:

- adding an optional output field;
- changing private computation while preserving the declared result;
- adding a new result shape behind a new surface or mutation name.

Breaking changes include:

- removing or renaming an output member without a declared alias;
- changing exposed row identity;
- making a required output optional;
- changing explicit sort semantics;
- widening an exhaustively declared enum result;
- removing a view or mutation bound by a module interface.

Changes to which rows occur in a view, when a mutation succeeds, or which error it returns are compatibility-relevant only to the extent that the boundary contract declares those properties. Package authors use a new major whenever such a declared promise is narrowed.

### E.6 Private model evolution

A private model may change freely when migration produces valid owned state and every boundary contract remains satisfied. Compatible private changes include adding or replacing fields, collections, computed values, helper mutations, checks, indexes, materialization hints, and internal algorithms.

Changes to keys, refs, bucket identity, module composition, or delete behavior require migration and complete prospective-state validation. They become boundary-breaking only when they alter an exposed identity, invalidate a required interface binding, strengthen a declared requirement, or remove a value or operation promised by a boundary contract.

A module update migrates only the state owned by that package instance. Parent and peer state remains available solely through the exact boundary views and mutations bound to the module.

### E.7 Mutation contracts

Parameter and response types are inferred before comparison. The checker compares the effective typed contract independently of whether the source wrote an explicit prototype.

An exposed mutation name MUST remain bound to a mutation satisfying its declared boundary contract. Private statements, assertions, algorithms, reads, and writes MAY change within that contract. Removing the binding, narrowing declared input, changing exposed receiver identity, or narrowing the declared response is breaking.

### E.8 Namespace and host-capability compatibility

A namespace requirement MAY move to a compatible minor or patch descriptor within the same namespace major. A new required capability is compatible for an existing boundary only when the earlier contract already required it. Requirements used solely by new or private behavior do not narrow an existing boundary.

The namespace interface hash covers typed functions, effect classes, codecs, and provider capabilities visible to package validation.

### E.9 Update diagnostics

Publication and load reports identify every mechanically detected narrowing path:

```text
package/version
surface or interface
old contract
new contract
variance rule
source span
```

A rejected publication creates no published release. A rejected load or update leaves the current package, bindings, and state active.

---
