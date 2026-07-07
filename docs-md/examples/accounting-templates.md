# Example: accounting templates

The original v0.4 archive includes a compact accounting-template module example. It demonstrates a host company that has a module space and a data-pack module that contributes accounting templates.

Use the normative example when exact syntax matters:

- `spec/examples/accounting-templates.md`
- Generated HTML: `spec/examples/accounting-templates.html`

## Shape of the example

- The host app owns `companies`.
- Each company has a local `modules` space.
- A data-pack module installs template rows into that space.
- The host can view the exposed templates and import one selected template.

## Why this matters

The example tests the core module rule: installed packages can add local data and surfaces, but host/module authority remains explicit.
