# Tutorial: task app

This tutorial shows the minimum useful Liasse app shape.

## Complete source

```hjson
$liasse: 1
$app: acme.tasks@0.1.0

$model: {
  users: {
    $key: id
    id: { $type: uuid, $default: "= uuid()" }
    email: { $type: text, $normalize: "lower(trim(.))" }
    name: text
  }

  tasks: {
    $key: id
    id: { $type: uuid, $default: "= uuid()" }
    title: { $type: text, $normalize: "trim(.)", $check: ["size(.) > 0", "title required"] }
    done: { $type: bool, $default: false }
    owner: { $ref: "/users", $on_delete: restrict }
    created_at: { $type: timestamp, $default: "= now()" }

    $mut: {
      "rename({ title: text })": ["{ title = @title }"]
      "complete()": ["{ done = true }"]
      "reopen()": ["{ done = false }"]
    }
  }

  open_tasks: {
    $view: "/tasks[!done] { id, title, owner, created_at }"
  }

  $roles: {
    member: {
      $members: "/users::"
      me: { $view: "/users[actor] { id, email, name }" }
      tasks: {
        $view: "/tasks[owner == actor] { id, title, done, created_at }"
        $mut: [rename, complete, reopen]
      }
    }
  }
}

$data: {
  users: []
  tasks: []
}
```

## What it demonstrates

- `users` and `tasks` are keyed collections.
- `owner` is a ref and must declare `$on_delete`.
- `title` is normalized before checks and storage.
- `open_tasks` is a derived view.
- The `member` role is the only client API surface.
- Mutations are names exposed through that surface.

## Things this example intentionally omits

The example does not show module installation, blobs, metered limits, extraction bundles, or cross-module dependencies. Those features are easier to learn after the base model is clear.
