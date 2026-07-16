#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Deterministic replay (§22): the same request sequence, admitted under the
//! same generators against a fresh store, reproduces byte-identical committed
//! state — including generated `uuid()`/`now()` fields.

mod support;

use liasse_ident::NameSegment;
use liasse_runtime::{CallRequest, Engine, Value};
use liasse_store::{CollectionPath, InstanceStore, MemoryStore, StoredRow};
use liasse_value::Text;
use support::{generator, store, TASKS};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// The committed rows of a collection, in Annex B order, straight from the
/// store — the byte-level state two runs must agree on.
fn rows(engine: &Engine<MemoryStore>, collection: &str) -> Vec<StoredRow> {
    let path = CollectionPath::top(NameSegment::new(collection));
    engine
        .store()
        .scan(&path)
        .expect("scan")
        .into_iter()
        .map(|(_, row)| row)
        .collect()
}

/// Run the same three-task sequence against a fresh engine.
fn run() -> Engine<MemoryStore> {
    let mut generator = generator();
    let mut engine = Engine::load(store("replay"), TASKS, &mut generator).expect("load");
    for title in ["first", "second", "third"] {
        engine
            .call(&CallRequest::new("add_task").arg("title", text(title)), &mut generator)
            .expect("call");
    }
    engine
}

#[test]
fn identical_request_sequences_reproduce_identical_state() {
    let a = run();
    let b = run();
    assert_eq!(a.head(), b.head(), "same number of commits");
    assert_eq!(
        rows(&a, "tasks"),
        rows(&b, "tasks"),
        "committed rows are byte-identical, generated fields included"
    );
    // Sanity: three distinct rows with generated uuid keys were actually stored.
    assert_eq!(rows(&a, "tasks").len(), 3);
}
