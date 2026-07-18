#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team probe (§21.2): erasing a COMPOSITE-keyed row must produce a durable
//! extract that captures the erased row's retained payload, exactly as a
//! single-field-keyed erase does.
//!
//! §21.2 step 2 requires the erase to capture each targeted live row's retained
//! payload before removal — that payload is what a §21.3 reinsertion restores,
//! so the extract's content hash (§21.2 step 4 / §21.3) is derived from the
//! payload. It follows, purely from the spec, that two rows sharing a composite
//! key but differing in a non-key field MUST yield DIFFERENT extracts: the
//! extract carries the whole payload, not merely the key. If a composite erase
//! instead recorded no payload, both would collapse to the identical
//! empty-material extract hash — the payload would be lost and the row could not
//! be reinserted.
//!
//! The `exec_erase` interpreter path addressed the targeted row by wrapping the
//! whole application-visible composite key (`Value::Composite([region, code])`)
//! as a ONE-component `KeyValue`, which never equals the stored row's
//! N-component `RowAddress` (`{ region } :: [code]`). The lookup therefore
//! missed, no occurrence was recorded, and the extract came back empty — the
//! payload silently dropped — while the live-state removal (keyed by the
//! un-wrapped `Value::Composite`) still succeeded. The scalar analogue works, so
//! this is a composite-key addressing defect. Expectations are re-derived from
//! §21.2, not the implementation.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, store};

/// A composite-keyed `regions` collection carrying a non-key `secret` payload
/// field, plus an insert and an inspection view. `erase_region` is exactly the
/// §21.2 corpus shape (`return erase(row)`) against a composite-key selection.
const REGIONS: &str = r#"{
  "$liasse": 1,
  "$app": "t.comperase@1.0.0",
  "$model": {
    "regions": { "$key": ["region", "code"], "region": "text", "code": "text", "secret": "text" },
    "all": { "$view": ".regions { region, code, secret, $sort: [region, code] }" },
    "$mut": {
      "add": ".regions + { region: @region, code: @code, secret: @secret }",
      "erase_region": "return erase(.regions[{ region: @region, code: @code }])"
    }
  }
}"#;

fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}

fn region_count(engine: &Engine<MemoryStore>) -> usize {
    engine.view_at_head("all").expect("view").expect("declared").rows().len()
}

/// Load the fixture with one region `[region, code]` bearing `secret`.
fn with_region(label: &str, region: &str, code: &str, secret: &str) -> Engine<MemoryStore> {
    let mut g = generator();
    let mut engine = Engine::load(store(label), REGIONS, &mut g).expect("load");
    let add = CallRequest::new("add")
        .arg("region", text(region))
        .arg("code", text(code))
        .arg("secret", text(secret));
    assert!(
        matches!(engine.call(&add, &mut g).expect("add"), CallOutcome::Committed { .. }),
        "the fixture seeds one composite-keyed region",
    );
    assert_eq!(region_count(&engine), 1, "one region seeded");
    engine
}

/// Erase the composite row `[region, code]` and return the durable extract's
/// content-hash response text (§21.2 step 6).
fn erase_region_hash(engine: &mut Engine<MemoryStore>, region: &str, code: &str) -> String {
    let mut g = generator();
    let request = CallRequest::new("erase_region").arg("region", text(region)).arg("code", text(code));
    let outcome = engine.call(&request, &mut g).expect("erase call");
    let CallOutcome::Committed { response, .. } = outcome else {
        panic!("erasing an extant composite row must commit a state change, got {outcome:?}");
    };
    let response = response.expect("erase returns the extract as its response");
    let wire = response.to_wire();
    wire.as_str().expect("the extract response is a content-hash text").to_owned()
}

#[test]
fn composite_erase_extract_captures_the_row_payload() {
    // Two engines, each with the IDENTICAL composite key [eu, x] but a DIFFERENT
    // non-key `secret`. §21.2 step 2 makes the extract carry the row's payload,
    // so the two extracts MUST differ — the differing bytes are the secret, which
    // is not part of the key. (Before the fix both composite erases recorded no
    // payload and collapsed to the same empty-material extract hash.)
    let mut engine_a = with_region("erase-a", "eu", "x", "alpha-secret");
    let mut engine_b = with_region("erase-b", "eu", "x", "beta-secret");

    let hash_a = erase_region_hash(&mut engine_a, "eu", "x");
    let hash_b = erase_region_hash(&mut engine_b, "eu", "x");

    // §21.2 step 1: the composite row is genuinely gone from live state.
    assert_eq!(region_count(&engine_a), 0, "the erased composite row is removed from live state");
    assert_eq!(region_count(&engine_b), 0, "the erased composite row is removed from live state");

    // The extract crosses the boundary as its content hash (§21.3): non-empty and
    // never re-leaking the scrubbed secret.
    assert!(!hash_a.is_empty(), "the composite-erase extract carries a content hash");
    assert!(!hash_b.is_empty(), "the composite-erase extract carries a content hash");
    assert!(!hash_a.contains("alpha-secret"), "the scrubbed secret never re-leaks through the response");
    assert!(!hash_b.contains("beta-secret"), "the scrubbed secret never re-leaks through the response");

    // §21.2 step 2: the extract captures the whole payload, so two rows sharing
    // the composite key [eu, x] but differing only in the non-key `secret` yield
    // DIFFERENT extracts. Equality here means the payload was dropped and the
    // extract came back empty — the bug.
    assert_ne!(
        hash_a, hash_b,
        "§21.2 step 2: the composite-erase extract must capture the row's payload (its `secret`); \
         identical hashes mean the payload was lost and the extract is empty",
    );
}
