# Feature map

This page gives a short explanation of every major feature and points to the normative spec page that defines it precisely.

## Authoring and package format

A Liasse package is a strict JSON tree after parsing. Hjson is recommended for hand-authored source because it keeps examples readable. Canonical validation, hashing, and loading happen after Hjson has been normalized to strict JSON.

Spec: `spec/README.md`, `spec/SYNTAX.md`, `spec/COLLECTIONS.md`.

## Types

Types include primitives, named shapes, enums, refs, sets, collection keys, view streams, structural objects, and optional values. Stored unique membership uses `$set`; ordered stored lists should be modeled as child rows unless the field is explicitly `json`.

Spec: `spec/SYNTAX.md`.

## Collections and structs

A plain object is a static struct unless it contains a collection/view/module/set marker. A keyed collection owns row identity with `$key`. Nested collections are logically nested even if a backend lowers them to relational tables.

Spec: `spec/COLLECTIONS.md`.

## Views

Views are typed row streams. They can filter, project, bind rows, group, sort, bound, and compose. View identity matters because mutation calls and client updates need stable row references.

Spec: `spec/SYNTAX.md`, `spec/COLLECTIONS.md`.

## Refs and delete behavior

Refs point at collection keys. There is no implicit `$on_delete` default. Any code path that can delete a target must prove that every incoming ref has an explicit policy such as `restrict`, `cascade`, or assignment to an expression.

Spec: `spec/SYNTAX.md`, `spec/MUTATIONS.md`, `spec/HISTORY.md`.

## Mutations

Mutations use target-first patch syntax. A mutation call is a transaction evaluated against a derived read basis. Successful calls admit a meaningful commit; stale, invalid, or no-op proposals do not create empty history entries.

Spec: `spec/MUTATIONS.md`, `spec/SYNTAX.md`.

## Checks, defaults, and normalization

Checks reject invalid values. Normalizers rewrite values before storage. Defaults and seed data are literal-or-expression positions, so strings beginning with `=` are expressions unless escaped.

Spec: `spec/CHECKS-TRANSFORMS.md`, `spec/SYNTAX.md`.

## Permissions and sessions

Users are rows. Roles grant surfaces. A client receives only the views and mutation names granted by its current role membership. Sessions and login are modeled rather than hidden in side channels.

Spec: `spec/PERMISSIONS.md`, `spec/CLIENT.md`.

## Modules

Modules install into module spaces, import explicit surfaces, expose interfaces, and can have required or optional dependencies. Optional import absence does not migrate private stored data. `$if` deactivation archives/restores data owned by the inactive declaration block.

Spec: `spec/MODULES.md`.

## Limits and sources

Meters model capacity and consumption. Sources have explicit order, eligibility, hierarchy, and module-boundary behavior. Cross-module spending and capacity are never inferred.

Spec: `spec/LIMITS.md`.

## Blobs and storage

`blob` is a primitive with content identity and metadata. `$blob_storage` declares placement requirements. The commit gate ties blob availability to data admission so metadata and bytes do not drift.

Spec: `spec/STORAGE.md`.

## History and extraction

Commits form a DAG. The `$history` surface exposes model-visible history. Extraction is a revertible delete that can produce bundles for reinsertion when allowed.

Spec: `spec/HISTORY.md`.

## Client protocol

The engine serves the client manifest from granted surfaces. Clients call declared names with parameters and subscribe to live views. Untrusted clients never submit arbitrary expressions.

Spec: `spec/CLIENT.md`.
