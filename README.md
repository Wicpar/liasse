# Liasse

Documentation and the v0.4 specification for **Liasse**.

## Contents

- **`spec/`** — the normative v0.4 specification (the standard).
- **`docs/`** — the compiled static documentation site served by GitHub Pages.
- **`docs-md/`** — the Markdown source for the documentation site.
- **`tools/build_site.py`** — regenerates `docs/` from `docs-md/` using only the Python standard library.

## Documentation site

The site is published from the `docs/` directory by GitHub Pages via
`.github/workflows/pages.yml`, which runs on every push to `main`.

Preview it locally:

```bash
python3 -m http.server 8000 --directory docs
# then open http://localhost:8000
```

## Editing the docs

Edit the Markdown under `docs-md/`, then regenerate the site:

```bash
python3 tools/build_site.py
```

Commit the regenerated `docs/` alongside your `docs-md/` changes.
