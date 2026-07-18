#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §12.2 apply-vs-recompute coherence: after each mutation the client's *applied*
//! replica equals the server's *recomputed* authorized view, across sort-changing
//! rename, remove, and insert-before mutations — the ordered patch is coherent, not
//! just eventually consistent.

mod support;

use liasse_wire::serde_json::json;
use support::{
    Client, app, call, drain, hello, server_titles, task_id_json, view,
};

#[test]
fn init_and_patches_track_the_sorted_view() {
    let mut core = app();
    let conn = hello(&mut core);
    view(&mut core, &conn, "w1", "public.tasks");

    let mut client = Client::new();
    client.feed(&drain(&mut core, &conn));
    assert!(client.titles("w1").is_empty(), "the initial view is empty");

    // insert "m", then "a" which sorts before it (insert-before).
    call(&mut core, &conn, "public.tasks.add", json!({ "title": "m" }), None);
    client.feed(&drain(&mut core, &conn));
    assert_eq!(client.titles("w1"), ["m"]);

    call(&mut core, &conn, "public.tasks.add", json!({ "title": "a" }), None);
    client.feed(&drain(&mut core, &conn));
    assert_eq!(client.titles("w1"), ["a", "m"], "the new row sorts before the existing one");

    // a sort-changing rename: m -> z re-orders to the tail (a move).
    let m = task_id_json(&core, "m");
    call(&mut core, &conn, "public.tasks.rename", json!({ "id": m, "title": "z" }), None);
    client.feed(&drain(&mut core, &conn));
    assert_eq!(client.titles("w1"), ["a", "z"], "the sort-changing update re-orders");

    // remove the first row.
    let a = task_id_json(&core, "a");
    call(&mut core, &conn, "public.tasks.remove", json!({ "id": a }), None);
    client.feed(&drain(&mut core, &conn));
    assert_eq!(client.titles("w1"), ["z"]);

    // the applied replica equals the recomputed authorized view at every step.
    assert_eq!(client.titles("w1"), server_titles(&core, &conn, "w1"));
}
