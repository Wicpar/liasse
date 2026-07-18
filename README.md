# Liasse

Liasse is a Rust-first application state engine. A package declares typed logical application state together with the operations and external interfaces that observe or change it. The specification defines values, identity, constraints, authorization, atomic admission, ordering, history, and client-visible coherence — while leaving storage, compilation, caching, partitioning, process placement, and resource allocation to the implementation.

## Specification

The complete v0.5 consolidated specification lives in [SPEC.md](SPEC.md). Status: **standard draft**.

| Part | Contents |
| --- | --- |
| I — Package and application model | Package structure, state model, expressions, views, mutations and validation, loading and bootstrapping |
| II — External API and clients | External interfaces and roles, authentication and sessions, clients and live views |
| III — Composition and advanced features | Modules, buckets, meters, host namespaces, keyrings and key providers, blobs |
| IV — History and application lifecycle | History, artifacts and reconciliation, package evolution and migrations, deletion and erasure |
| V — Runtime and host contract | Runtime semantics, Rust host and implementation contract |
| VI — Worked examples | Task API, passkey login and sessions, subscription credits, template modules |
| VII — Normative annexes | Types and wire values, deterministic total order, grammar and syntax index, canonical identity and integrity, package compatibility |

## A taste

A minimal package defines data, behavior, a view, and a public API in one model:

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

Clients **call** mutations and **watch** views. Every successful mutation is one final, atomic commit, and every live view on the same client connection advances through it.

## Core concepts

- **Package** — declares typed state, views, mutations, and surfaces; built into a portable `.liasse` artifact.
- **State model** — a logical tree of scalar fields, structs, keyed collections, and sets with application-defined identity.
- **View** — a typed read-only result derived from state, watchable as a live subscription.
- **Mutation** — a typed sequential program proposing one atomic state transition; admission validates and integrates it at one final serial position.
- **Surface** — a named external API entry; authentication resolves an actor row, and roles determine which surfaces an actor may use.
- **History** — every commit is final and retained per policy; artifacts capture a selected state and history for export, restore, and reconciliation.

## Conformance

The spec defines four conformance classes: **packages**, **runtimes**, **client bindings**, and **host components**. A runtime claiming Liasse v0.5 support must implement the complete normative language of the specification.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
