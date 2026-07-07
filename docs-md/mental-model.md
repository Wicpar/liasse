# Mental model

Liasse is easiest to understand as five layers that are checked together.

## Package tree

A package has a `$model` and optional `$data`. The logical model is a tree of fields, structs, collections, module spaces, views, and surfaces. Storage may be relational, but backend flattening is invisible to authors.

## Rows and identity

A keyed collection owns rows. A row identity is local to the collection and its parent path. Refs point to collection keys, not to backend table rows.

## Views and surfaces

A view is data derived from data. A surface is the subset of views and mutation names granted to a role or imported by a module. The engine serves clients from surfaces rather than from handwritten REST or GraphQL routes.

## Mutations and commits

A mutation evaluates against a read basis. The engine admits either one meaningful commit or rejects/marks the proposal unchanged. No commit is created just to say that nothing happened.

## Modules

A module is installed inside a module space. It can import parent-provided surfaces, expose interfaces, and be updated only when structural checks pass. Cross-module authority is explicit at every boundary.

## History and extraction

History is modeled as data. Commits form a DAG. Erasure is modeled as extraction: data can leave the live model while preserving enough bundle information to reinsert it if policy allows.
