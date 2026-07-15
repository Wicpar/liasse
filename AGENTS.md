# AGENTS.md

Binding instructions for anyone — human or agent — writing code in this repository.

## Writing Rust code

- Structure state so that invalid state is unrepresentable in the type system.
- Do not validate; parse. Turn raw input into a richer type once, at the boundary; past that boundary the type is proof the data is well-formed.
- Use semantic types: types that have meaning and group reusable functionality, instead of abusing meaningless scalars.
- Do not use interior-mutability structures. Avoid reference counts. They are a sign of a bad design; normally everything is expressible as owned values, `&`, and `&mut`.
- Code must never panic. The exception is unrecoverable states, like poisoned locks in some cases. Tests are expected to panic on failed cases.
- Keep it simple: if it's long and complicated, you are missing an abstraction and should research better representations. Leverage the Rust type system to create safe abstractions that have pre-validated states instead of propagating `Result`/`Option` everywhere.
- Do not reinvent the wheel; try to find existing crates that do the job.
- When parsing a language, emit rustc-like helpful diagnostics. A diagnostic MUST allow a new user to understand what went wrong, why, and how to fix it (when a hint is available).
- Do not write noisy code. Noisy code is code with a lot of busy work that does not contribute to the core logic of what is being programmed. Keep things in abstractions that allow functionality to be implemented at homogeneous abstraction levels. Good code is code where each statement truly contributes to the understanding of what is being computed.
- Avoid creating bare `fn` helpers; most of the time there is a type representing state on which to `impl` the functions, or traits to implement.
- When multiple structures implement common functionality, and code relies on that functionality without caring about the backing structure, use traits.
- Code files should never be longer than a few hundred lines. A file growing past that is a missing module boundary or a missing abstraction.

## Writing tests

- Tests verify that logical invariants are not broken.
- Never write tests for invariants the type system already enforces.
- Never write tests that measure performance.
- Tests must verify that parts of the program correctly interact.
- Never write tautological tests that rely on the program's own answer not changing. The expected result must be externally deducible for the test to be useful.
- Do not put tests in the same file as the functionality; create separate test files containing only tests.

## Benchmarks

- Benchmark functionality with criterion at every level, low and high.
- Every non-trivial custom abstraction, algorithm, or data structure should benchmark all the different performance axes it has.
- Do not benchmark trivial or crate-imported functionality. The only exception is when choosing between multiple candidate crates.

## Project constraints

- The parser MUST be built on `pest`.
- The storage backend is PostgreSQL (`liasse-pg` is the only crate that talks to it; everything else goes through the `liasse-store` contract).
- The normative reference is [SPEC.md](SPEC.md) (Liasse v0.5). Every observable behavior traces to a spec rule.
- The conformance corpus lives in `tests/` (file-based; format defined in [tests/FORMAT.md](tests/FORMAT.md)). The corpus is written **before** implementation: implementation work makes existing cases pass — never rewrite a case to match implementation behavior unless the case itself contradicts SPEC.md.
