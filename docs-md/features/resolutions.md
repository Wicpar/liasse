# Resolved decisions

The resolutions file records design decisions that were ambiguous during v0.4.

Most important locked decisions:

- Hjson is source sugar; canonical artifacts are strict JSON.
- There is no implicit `$on_delete` default.
- Proposal failures and no-ops do not create commits.
- Author-level `at` was removed in favor of engine/client history surfaces.
- `secret()` is not a language-level generative function.
- `none` is the no-value sentinel; JSON null is not used for absence.
