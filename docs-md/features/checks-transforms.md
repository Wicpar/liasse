# Checks and transforms

## Defaults

Defaults fill absent values. They may be literals or marked expressions.

## Normalization

A normalizer rewrites a value before storage. Common examples are trimming text or canonicalizing email case.

## Checks

Checks evaluate after normalization. Failed checks reject the write and should carry a user-visible message.

## Migration transforms

Transforms describe how existing stored data moves when a model changes. They must be replayable and checked, not ad-hoc scripts hidden outside the model.
