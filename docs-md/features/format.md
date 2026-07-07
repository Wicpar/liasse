# Authoring format

## Rule

The canonical artifact is strict JSON. Hjson is a source format accepted by tools before validation. After parsing, every Hjson file must produce the same strict JSON package tree that the engine would accept directly.

## Why Hjson

Liasse examples contain long view and mutation expressions. Hjson keeps those readable with comments and multiline strings while preserving one canonical engine input.

## Escape rule

In literal-or-expression positions, a leading `=` marks an expression. A leading apostrophe forces a literal.

```hjson
formula: "'= total_ttc"   # stored text: = total_ttc
default_id: "= uuid()"    # expression
text: "'hello"            # stored text: hello
quote: "''hello"          # stored text: 'hello
```

## Avoid

Do not rely on Hjson null. Liasse uses `none` for absent/no-value. JSON null is only valid inside fields explicitly typed as `json`.
