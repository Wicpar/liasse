#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §21.2 red team: the surface `erase` verb must, on a committed removal, bind a
//! durable extract of the removed row's payload (step 2 "creates a durable
//! extract containing every payload required for possible reinsertion", step 6
//! "admits the erasure commit and returns the extract").
//!
//! `SurfaceHost::erase` synthesizes that extract by diffing the surface `$view`'s
//! rows before/after the removal. Diffing by *value* would lose row multiplicity:
//! under a NON-INJECTIVE projection (a view that hides the key), two distinct rows
//! can project to the same field map, and a value-equality diff then lets a
//! surviving sibling mask a removed row, binding NO extract for a real removal.
//! The diff keys on each row's stable `RowId` identity instead, so the co-projecting
//! sibling cannot mask the removal.

mod support;

use liasse_store::MemoryStore;
use liasse_surface::{
    CallBinding, Engine, Precision, SurfaceBinding, SurfaceHost, SurfaceOutcome, SurfaceRouter,
    SurfaceRouterBuilder, ViewBinding, VirtualClock,
};
use support::{call, store, text, NOW};

/// A minimal app whose PUBLIC tasks surface exposes a NON-INJECTIVE view
/// (`titles` projects only `title`, not the `id` key). `index` (id+title) is
/// kept only so the harness can count rows.
const APP: &str = r#"{
  "$liasse": 1
  "$app": "probe.erase@1.0.0"
  "$model": {
    "tasks": { "$key": "id", "id": "text", "title": "text" }
    "index": { "$view": ".tasks { id, title, $sort: [id] }" }
    "titles": { "$view": ".tasks { title, $sort: [title] }" }
    "$mut": {
      "add": ".tasks + { id: @id, title: @title }"
      "remove": ".tasks - @id"
    }
    "$public": {
      "tasks": {
        "$view": ".titles"
        "$mut": { "add": ".add", "remove": ".remove" }
      }
    }
  }
}"#;

fn build_router(model: &liasse_model::Model) -> SurfaceRouter {
    let public_tasks = SurfaceBinding::new()
        .with_view(ViewBinding::new("titles"))
        .with_call("add", CallBinding::root("add", ["id".to_owned(), "title".to_owned()]))
        .with_call("remove", CallBinding::root("remove", ["id".to_owned()]));
    SurfaceRouterBuilder::new()
        .public_surface("tasks", public_tasks)
        .build(model)
        .expect("router validates against the model")
}

fn probe_host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = match Engine::load(store("probe-erase"), APP, &mut clock) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error}"),
    };
    let router = build_router(engine.model());
    SurfaceHost::new(engine, router, clock)
}

fn add(host: &mut SurfaceHost<MemoryStore>, id: &str, title: &str) {
    let outcome = host
        .call("c1", &call("public.tasks.add", [("id", text(id)), ("title", text(title))]))
        .expect("add drives");
    assert!(matches!(outcome, SurfaceOutcome::Committed { .. }), "add commits: {outcome:?}");
}

/// Rows currently in the internal `index` view (proves the removal committed).
fn task_count(host: &SurfaceHost<MemoryStore>) -> usize {
    host.engine().view_at_head("index").expect("view").expect("declared").rows().len()
}

/// A uniquely-projected row: the committed erasure binds an extract.
#[test]
fn unique_projection_binds_extract() {
    let mut host = probe_host();
    host.connect("c1");
    add(&mut host, "r1", "alpha");
    add(&mut host, "r2", "beta");

    let erased = host
        .erase("c1", &call("public.tasks.remove", [("id", text("r1"))]))
        .expect("erase drives");
    assert!(matches!(erased.outcome(), SurfaceOutcome::Committed { .. }), "removal commits");
    assert_eq!(task_count(&host), 1, "one row removed");
    assert!(
        erased.extract().is_some(),
        "a committed erasure of a uniquely-projected row binds an extract",
    );
}

/// §21.2 step 2/6: two rows share the projected map `{title: dup}`. Erasing r1
/// commits a real removal (task_count drops 2 -> 1); the identity diff captures
/// r1 even though r2 co-projects its map, so the committed erasure binds an
/// extract.
#[test]
fn committed_erasure_of_coprojecting_row_binds_extract() {
    let mut host = probe_host();
    host.connect("c1");
    add(&mut host, "r1", "dup");
    add(&mut host, "r2", "dup");
    assert_eq!(task_count(&host), 2, "two rows before the erase");

    let erased = host
        .erase("c1", &call("public.tasks.remove", [("id", text("r1"))]))
        .expect("erase drives");

    assert!(
        matches!(erased.outcome(), SurfaceOutcome::Committed { .. }),
        "the erase committed a real removal: {:?}",
        erased.outcome()
    );
    assert_eq!(task_count(&host), 1, "exactly one row was removed by the erase");

    assert!(
        erased.extract().is_some(),
        "§21.2 step 2/6: a committed erasure must bind a durable extract for the \
         removed row even when a surviving sibling projects to the same field map",
    );
}
