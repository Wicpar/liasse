# Refs

Refs are checked key values that point to rows in a collection or surface.

```hjson
owner: { $ref: "/users", $on_delete: restrict }
```

## Delete completeness

There is no implicit delete behavior. If target deletion is possible, every incoming ref must choose a policy. This includes mutations exposed across module boundaries: the module that exposes or admits a deleting mutation must still prove delete-policy completeness.

## No value

The absent/no-value sentinel is `none`, not JSON null.
