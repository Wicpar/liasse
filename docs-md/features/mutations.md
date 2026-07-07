# Mutations

Mutations use target-first syntax so the thing being changed is visible first.

```hjson
"rename({ title: text })": ["{ title = @title }"]
```

## Patch block

`field = expr` sets a value. `-field` clears/removes where legal. Collection operators insert, replace, or delete rows.

## Atomicity

A sequence of mutation expressions has one atomic outcome. Stale read basis, failed checks, insufficient capacity, and invalid ref behavior reject the proposal. No-op proposals are not admitted as empty commits.

## Exposed calls

Clients and modules call names, not arbitrary expressions. The engine resolves the name to a declared mutation under the authority of the granted surface.
