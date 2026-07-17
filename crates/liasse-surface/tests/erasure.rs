#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §21.2 erasure as a surface host verb: an erasure surface call routes the live
//! removal through ordinary admission, so the erased row is unobservable in the
//! live view AND absent from a fresh export, and the verb binds the durable
//! extract the driver reads back.
//!
//! The fixture routes the erasure surface to the delete-backed `public.tasks.remove`
//! (`.tasks - @id`): in CORE scope an erasure's live effect is exactly an ordinary
//! deletion (§21.2 step 1), so this exercises the surface's whole contribution —
//! committing the removal and synthesizing the extract — end to end. The runtime
//! `erase(row)` builtin that would drive a call literally bound to `erase(.coll[@id])`
//! is a documented seam (see `src/host/erasure.rs`).

mod support;

use liasse_store::MemoryStore;
use liasse_surface::{Engine, Precision, SurfaceOutcome, Value, VirtualClock};
use support::{add_task, call, host, store, text, NOW};

/// The titles currently in the public `index` view.
fn view_titles(engine: &Engine<MemoryStore>) -> Vec<Value> {
    engine
        .view_at_head("index")
        .expect("view")
        .expect("declared")
        .rows()
        .iter()
        .filter_map(|row| row.field("title").cloned())
        .collect()
}

#[test]
fn erase_removes_row_from_view_and_from_a_fresh_export() {
    let mut host = host();
    host.connect("c1");
    let id = add_task(&mut host, "c1", "gamma");

    // The row is observable before the erasure.
    assert!(view_titles(host.engine()).contains(&text("gamma")), "row present before erase");

    // Erase it through the (delete-backed) erasure surface.
    let erased = host
        .erase("c1", &call("public.tasks.remove", [("id", id)]))
        .expect("erase drives");
    assert!(
        matches!(erased.outcome(), SurfaceOutcome::Committed { .. }),
        "the live removal commits: {:?}",
        erased.outcome()
    );

    // §21.2 step 6: the verb binds a durable extract with a verifiable content hash.
    let extract = erased.extract().expect("a committed erasure binds an extract");
    assert!(!extract.hash().is_empty(), "the extract carries a content hash");

    // Unobservable in the live view.
    assert!(
        !view_titles(host.engine()).contains(&text("gamma")),
        "erased row is gone from the live view"
    );

    // Absent from a fresh export -> restore (§19.8/§21.2): the export captures the
    // live state the removal already left the row out of, so a fresh instance
    // rebuilt from those bytes never sees it.
    let bytes = host.export().expect("export");
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let fresh = Engine::restore(store("fresh"), &bytes, &mut clock).expect("restore verifies");
    assert!(
        !view_titles(&fresh).contains(&text("gamma")),
        "erased row is absent from a fresh export"
    );
}

#[test]
fn erasing_an_absent_key_commits_nothing_and_binds_no_extract() {
    let mut host = host();
    host.connect("c1");
    // No such task: the routed removal changes nothing (§8.9), so there is no
    // scrubbed payload and therefore no extract (§21.2 extracts only on removal).
    let erased = host
        .erase("c1", &call("public.tasks.remove", [("id", text("no-such-id"))]))
        .expect("erase drives");
    assert!(matches!(erased.outcome(), SurfaceOutcome::Unchanged { .. }), "nothing removed");
    assert!(erased.extract().is_none(), "no extract without a removal");
}
