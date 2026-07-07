# Views

A view is a derived row stream.

```hjson
open_tasks: {
  $view: "/tasks[!done] { id, title, owner }"
}
```

## Projection

Inside `{ ... }`, `.` is the source row. Fields emitted earlier become local bindings for fields emitted later.

## Identity

A view may preserve source identity or synthesize new keyed rows. Identity must be stable enough for live updates, client windows, and mutation calls exposed through a surface.

## Bounds and windows

`$skip` and `$limit` are view bounds. Client windows are delivery policy; they do not change the underlying view semantics.
