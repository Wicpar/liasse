//! Model: parses a spanned syntax document into the validated semantic model of
//! a package's state, expressions, views, mutations, surfaces, and roles
//! (SPEC.md Part I–II static rules). A constructed [`Model`] is proof the
//! package is statically valid: parse, don't validate.
//!
//! # Pipeline
//!
//! [`Model::build`] runs one accumulate-then-report pass over the
//! [`liasse_syntax`] document:
//!
//! 1. **Header** (§4, §2.5, Annex E) — `$liasse` generation (checked first),
//!    the exclusive `$app`/`$module` identity, and the shape of `$semantics`,
//!    `$requires`, and `$resources`.
//! 2. **State tree** (§5) — fields, computed values, static structs, keyed
//!    collections (simple and composite `$key`), sets, enums, refs, `$unique`,
//!    and reusable `$types`, with the §2.5 name grammar, reserved/unknown-member
//!    rules, `$key`/A.8 key-eligibility, and enum distinctness enforced.
//! 3. **References** (§5.6) — every `$ref` resolves to a declared collection.
//! 4. **Expressions** (§5.1, §5.2, §5.10, §7) — defaults, computed values,
//!    `$normalize`, `$check`, and `$view` are typed in scope through
//!    [`liasse_expr`], and the default dependency graph is proven acyclic.
//! 5. **Mutations** (§8) — programs are parsed, parameters inferred (§8.3) or
//!    taken from a prototype, and statements checked (no write to a computed
//!    value, `return` last, `bool` assertions).
//! 6. **Surfaces** (§10) — `$public`/`$roles` shape, `$view` typing, and every
//!    `$mut` reference naming a declared mutation.
//! 7. **Seed** (§5, §9) — `$data` values decoded against their declared types.
//! 8. **Feature declarations** — the remaining declaration families' static
//!    rules, each in its own module: authenticators (§11, [`auth`]), buckets
//!    (§14, [`bucket`]), meters (§15, [`meter`]), keyrings (§17, [`keyring`]),
//!    blobs (§18, [`blob`]), module composition (§13, [`module`]), history
//!    policy (§19, [`history`]), migrations (§20, [`migration`]), and deletion
//!    policy (§21, [`delete`]). Each validates its Annex C grammar, types the
//!    expressions its scope makes checkable, and enforces the cross-model MUSTs
//!    its chapter pins (a role's `$auth` names a declared authenticator; a
//!    `$consumes` meter resolves on the ancestor chain; a deleting mutation
//!    forces every inbound ref to decide `$on_delete`).
//!
//! Every rejection is a [`liasse_diag`] diagnostic that says what is wrong,
//! points at the span, and offers a fix hint when one exists.
//!
//! # CORE scope
//!
//! The rules that need runtime machinery are modelled faithfully and left as
//! documented seams: host-namespace/provider/connector resolution (§16/§17/§18
//! capability checks), cross-package module composition and version
//! compatibility (§13, Annex E), pool typing through the `$quantity` projection
//! role and the parameterless-accessor rule (§15.6), unbounded-recurring bucket
//! enumeration (§14.5), and two-model migration typing with the reversible
//! round trip (§20.2). Recursive `$types` typing is depth-bounded, computed-value
//! types are not propagated into sibling row shapes, and full insert/replace/
//! delete result typing is partial; each is noted at its definition.

mod auth;
mod blob;
mod bucket;
mod build;
mod check;
mod delete;
mod doc;
mod expose;
mod header;
mod history;
mod host;
mod infer;
mod keyring;
mod meter;
mod migration;
mod model;
mod module;
mod mutation;
mod names;
mod refs;
mod report;
mod resolve;
mod scope;
mod seed;
mod state;
mod surface;
mod types;
mod walk;

pub use expose::{ExposedInterface, ExposedMut};
pub use header::{Header, Kind};
pub use host::HostDescriptors;
pub use model::Model;
pub use mutation::Mutation;
pub use names::{DeclName, PackageId, PackageName, Version};
pub use report::code;
pub use state::{
    Check, Collection, ExprSource, Member, Node, Reference, ScalarField, SetField, Shape, ViewDecl,
};
pub use surface::Surface;
