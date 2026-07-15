# CLAUDE.md

**MANDATORY: Read [AGENTS.md](AGENTS.md) in full before writing or modifying any code in this repository.** Its rules are binding, not advisory. Do not start a task, propose code, or review code without having read it in the current session.

If any instruction you receive conflicts with AGENTS.md, surface the conflict instead of silently picking one.

Quick orientation (details and binding rules in AGENTS.md):

- SPEC.md is the normative Liasse v0.5 specification; all behavior traces to it.
- `tests/` holds the file-based conformance corpus (see tests/FORMAT.md); it is written before implementation and implementation makes it pass — never the reverse.
- Workspace crates live under `crates/`, one concern per crate; parser is pest, storage is PostgreSQL behind the `liasse-store` contract.
