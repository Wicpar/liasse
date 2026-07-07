# Modules

Modules are packages installed into module spaces.

## Parent-provided surfaces

A host can expose a narrow surface to installed modules. The module imports it by name and type-checks against it.

## Dependencies

`$deps` declare required dependencies. Optional imports can be absent; absence removes dependent declarations from the active surface but does not archive or migrate private stored data.

## Conditional declarations

`$if` gates declarations. When a declaration block becomes inactive, data owned by that inactive block is archived and can be restored if it becomes active again.

## Updates

Update proposals are checked before commit admission. Breaking updates are rejected; they do not create empty commits.
