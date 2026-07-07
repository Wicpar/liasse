# Getting started

This page builds a tiny task app and introduces the core authoring shape.

## 1. Pick an authoring format

Canonical Liasse packages are strict JSON. Humans usually write Hjson because comments, unquoted keys, optional commas, and multiline strings make the source easier to read.

```hjson
$liasse: 1
$app: acme.tasks@0.1.0
$model: {}
$data: {}
```

Before loading, the engine or tooling parses the Hjson source to canonical JSON. Hjson never changes language semantics.

## 2. Declare rows

A keyed collection stores rows. The `$key` names the local identity field.

```hjson
$model: {
  tasks: {
    $key: id
    id: { $type: uuid, $default: "= uuid()" }
    title: { $type: text, $check: ["size(trim(.)) > 0", "title required"] }
    done: { $type: bool, $default: false }
  }
}
```

`$default` is a literal-or-expression position. A string beginning with `=` is evaluated as an expression. A literal string that must begin with `=` is escaped with a leading apostrophe: `"'= total"`.

## 3. Add data

`$data` uses the same literal-or-expression rule recursively.

```hjson
$data: {
  tasks: [
    { id: "8b707e1e-1aa8-4e19-a391-2d8f9fc2b7e5", title: "Read the spec", done: false }
  ]
}
```

## 4. Add a view

A view is a typed row stream derived by the engine.

```hjson
$model: {
  tasks: { ... }
  open_tasks: {
    $view: ".tasks[!done] { id, title }"
  }
}
```

In a projection block, `.` is the source row. Output fields become bindings as the block is built.

## 5. Add mutations

Mutations are declared on the shape whose data they are allowed to change.

```hjson
tasks: {
  $key: id
  id: { $type: uuid, $default: "= uuid()" }
  title: text
  done: { $type: bool, $default: false }

  $mut: {
    "rename({ title: text })": ["{ title = @title }"]
    "complete()": ["{ done = true }"]
  }
}
```

Each call is atomic. If the derived read basis is stale, or a check fails, the proposal is rejected and no empty commit is recorded.

## 6. Expose the client surface

A role grants views and callable mutation names. Clients send names and parameters; the engine resolves the granted declarations.

```hjson
$model: {
  users: {
    $key: id
    id: { $type: uuid, $default: "= uuid()" }
    email: text
  }

  tasks: { ... }

  $roles: {
    member: {
      $members: "/users::"
      tasks: {
        $view: "/tasks { id, title, done }"
        $mut: [rename, complete]
      }
    }
  }
}
```

The surface is the API. Anything not granted by a role is invisible to that client.

## 7. Next step

Continue with the Tasks tutorial, then use the feature explanations when a construct appears for the first time.
