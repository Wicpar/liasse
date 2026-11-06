# Liasse

Liasse is a language for application state. A **module** declares stored
state, views that read it, and mutations that change it; an **instance** is a
module with its own state:

```liasse
meta { name: "acme.tasks", version: "1.0.0", language: "liasse 0.0.1" }

tasks: {
    pub title: Text
    pub done = false
    pub mut complete() { .done = true; }
}*
pub view open_tasks = .tasks[!.done]
pub mut add(title: Text) {
    return view .tasks += { title };
}
```

Every call is a transaction, and each instance keeps the history of its
commits. Clients watch views live: each commit sends them the change to their
results. A module's new version comes with a migration, and its instances move
to it.

This repository holds the definition of `liasse 0.0.1`.

## Getting started

Read [LANGUAGE.md](LANGUAGE.md) first. Its first chapter walks through the
module above.

| LANGUAGE | Defines |
| --- | --- |
| 1 | A first module, and the rules of the syntax. |
| 2 | Values, optional values and sets, and the state that a module stores. |
| 3 | Expressions: names, records, calls, operators, and pipelines over sets. |
| 4 | Functions and views, callbacks and generics, and ref views. |
| 5 | Mutations: statements, writes, patches, and the results that they return. |
| 6 | Templates and mixins. |
| 7 | Files, imports, interfaces, mounted modules, and host libraries. |
| 8 | Migrations between a module's versions. |
| Annex A | The complete grammar. |

Then read [SPEC.md](SPEC.md), which defines what every implementation of the
language shares.

| SPEC | Defines |
| --- | --- |
| 1 | Instances, transactions, and versions. |
| 2 | The builtins: types, declarations, content, and constraints. |
| 3 | Artifacts: packages, instances, snapshots, extraction, and import. |
| 4 | Values and the data file. |
| 5 | History: records, their trace and changes, and replay. |
| 6 | Conformance: the outcomes that every implementation produces. |
| Annex A | The history schema. |

Then read [LIBRARIES.md](LIBRARIES.md), the host libraries that the alpha
supplies to every module, beyond the builtins of SPEC chapter 2.

| LIBRARIES | Defines |
| --- | --- |
| 1 | Host types: bytes, UUIDs, dates, instants, and durations, with their order, limits, and operators. |
| 2 | The templates `Map` and `Enum`, and the calls that initialize and update instances. |
| 3 | Functions and mutations: text, conversions, signatures, past rows, time, sets, encodings, and the keyring. |
| 4 | The metaproperty `$history`: when a row was inserted and last changed. |
| 5 | The definitions of `Map` and `Enum`. |

## License

MIT OR Apache-2.0, at your option: [LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE).
