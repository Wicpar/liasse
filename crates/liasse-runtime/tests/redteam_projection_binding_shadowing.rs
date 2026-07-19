//! RED TEAM — §6.4 row bindings × §7.1 projection member scoping. Characterizes the
//! flagged, currently-UNPINNED ambiguity: when a projection defines an output member
//! whose name COLLIDES with the loop/row binding, which does a later RHS reference
//! resolve to — the loop binding (the row) or the just-defined output member?
//!
//! # Observed behaviour (fully characterized below)
//!
//! For `.companies[:s] { s: s.id, sn: s.name }`:
//!
//! - `s: s.id` — the RHS `s` resolves to the LOOP BINDING (the row); `.id` reads the
//!   row's id. (No error here.)
//! - `sn: s.name` — the RHS `s` resolves to the OUTPUT MEMBER `s` (a `text`, the id),
//!   so `.name` fails to LOAD: `E-EXPR "cannot read field \`name\` of a text"`.
//!
//! So an output member SHADOWS a same-named loop binding for OTHER members' RHS, but
//! NOT within its own RHS. Empirically:
//!
//! - The shadowing is ORDER-INDEPENDENT: `{ sn: s.name, s: s.id }` fails the same way
//!   as `{ s: s.id, sn: s.name }` — consistent with §7.1's "Projection members are
//!   unordered named outputs."
//! - It is COLLISION-SPECIFIC: rename the output member (`{ t: s.id, sn: s.name }`) or
//!   drop it (`{ sn: s.name }`) and `s.name` correctly reads the loop binding (the
//!   row's name). Rename the loop binding and the collision likewise vanishes.
//!
//! # Verdict — BUG-leaning; recommend pinning "loop binding wins"
//!
//! §6.4 states a row binding "names each row while a selector or **projection**
//! evaluates", so the loop binding is in scope for the WHOLE projection and `s`
//! should denote the row throughout. The current resolution is internally
//! INCONSISTENT: the same identifier `s` denotes the row inside member `s`'s own RHS
//! but the projected `text` inside member `sn`'s RHS — that inconsistency is the
//! strongest evidence the shadowing is unintended. The author-intended reading of
//! `.companies[:s] { s: s.id, sn: s.name }` is plainly "emit `s = the id`, `sn = the
//! name`", both derived from the row `s`. Under the recommended pin (loop binding
//! wins; an output member never shadows an in-scope row binding for a sibling RHS),
//! the view loads and `sn` reads the row's name.
//!
//! This is genuinely UNPINNED in SPEC (§7.1 says members MAY refer to one another
//! when acyclic, but does not say an output name shadows an in-scope loop binding),
//! so the collision case is filed as an `#[ignore]`d PROPOSED-PIN repro, not a hard
//! spec-repro. The passing controls assert only the parts that ARE deducible from
//! §6.4 (no-collision projections read the loop binding).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_ident::InstanceId;
use liasse_runtime::{CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::generator;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// Build the single-collection app whose view is `view_expr`, insert one company
/// `{ id: "c1", name: "ACME" }`, and return the first view row's `sn` field (if the
/// package loaded at all). `Err` carries the load-error debug string.
fn sn_of(instance: &str, view_expr: &str) -> Result<Option<Value>, String> {
    let def = format!(
        r##"{{
          "$liasse": 1, "$app": "t.shadow@1.0.0",
          "$model": {{
            "companies": {{ "$key": "id", "id": "text", "name": "text" }},
            "v": {{ "$view": "{view_expr}" }},
            "$mut": {{ "add": ".companies + {{ id: @id, name: @name }}" }}
          }}
        }}"##
    );
    let mut generator = generator();
    let store = MemoryStore::new(InstanceId::new(instance));
    let mut engine = Engine::load(store, &def, &mut generator).map_err(|e| format!("{e:?}"))?;
    engine
        .call(&CallRequest::new("add").arg("id", text("c1")).arg("name", text("ACME")), &mut generator)
        .expect("dispatch");
    let view = engine.view_at_head("v").expect("view ok").expect("view declared");
    Ok(view.rows()[0].field("sn").cloned())
}

// ===========================================================================
// PASSING CONTROLS — deducible from §6.4 (the loop binding names the row for the
// whole projection). These lock that a NON-colliding projection reads the row.
// ===========================================================================

/// CONTROL: no output member collides with the loop binding `s`; `sn: s.name` reads
/// the LOOP BINDING (the row) and yields the row's name.
#[test]
fn control_no_collision_reads_loop_binding() {
    let sn = sn_of("shadow-nocollide", ".companies[:s] { t: s.id, sn: s.name }")
        .expect("a non-colliding projection must load");
    assert_eq!(sn, Some(text("ACME")), "`sn: s.name` must read the loop binding (the row's name)");
}

/// CONTROL: with only `sn: s.name` and no output member named `s`, the loop binding
/// is read; the view loads and yields the row's name.
#[test]
fn control_only_sn_reads_loop_binding() {
    let sn = sn_of("shadow-onlysn", ".companies[:s] { sn: s.name }")
        .expect("a projection reading only the loop binding must load");
    assert_eq!(sn, Some(text("ACME")), "`sn: s.name` must read the loop binding (the row's name)");
}

/// CONTROL: a renamed loop binding removes the collision — `{ s: r.id, sn: r.name }`
/// over `[:r]` loads and reads the row. Proves the break is the NAME COLLISION with
/// the loop binding specifically, and that renaming the binding is the workaround.
#[test]
fn control_renamed_binding_avoids_collision() {
    let sn = sn_of("shadow-renamed", ".companies[:r] { s: r.id, sn: r.name }")
        .expect("a renamed loop binding avoids the collision and loads");
    assert_eq!(sn, Some(text("ACME")), "with the binding renamed, `sn: r.name` reads the row's name");
}

// ===========================================================================
// PINNED (§7.1/§6.4) — the flagged collision case. Asserts the pinned
// resolution: a sibling output member never shadows an in-scope loop binding.
// ===========================================================================

/// §6.4: the loop binding `s` names the row for the whole projection, so in
/// `.companies[:s] { s: s.id, sn: s.name }` the RHS `s.name` reads the row and
/// yields its name — the view loads. §7.1 (pinned): an output member name never
/// shadows an in-scope row binding when evaluating a sibling member's expression.
/// Before the pin this was rejected at load with `E-EXPR "cannot read field
/// \`name\` of a text"` because the output member `s` (the id text) shadowed the
/// loop binding for the sibling RHS.
#[test]
fn collision_rhs_should_read_loop_binding() {
    let sn = sn_of("shadow-collide", ".companies[:s] { s: s.id, sn: s.name }")
        .expect("under the recommended pin the colliding projection loads");
    assert_eq!(sn, Some(text("ACME")), "`sn: s.name` should read the loop binding (the row's name), not the output member `s`");
}
