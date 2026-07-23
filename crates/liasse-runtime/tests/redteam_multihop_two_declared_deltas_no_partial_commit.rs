#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §20.1/§20.3/Annex E.9 red-team: a MULTI-HOP upgrade route — the active source
//! HAS a declared delta, but a declared `$migrations` key sits strictly between the
//! active and target versions, so the route requires composing more than one hop —
//! must be REJECTED for in-place update, not silently committed after running only
//! the single active-source delta.
//!
//! # What the spec requires
//!
//! §20.1 route resolution: each declared key's delta "bridges that version to the
//! next declared key, and the greatest key's delta bridges to the package's own
//! version"; "composition of a multi-step route is exactly the declared chain,
//! never an implementation option. The runtime MUST NOT synthesize an undeclared
//! intermediate version." Multi-step chain walking is a documented unbuilt hole
//! (SPEC-ISSUES #22), so the runtime cannot compose such a route; the only
//! spec-legal action left is fail-closed refusal (§9.4, Annex E.9), keeping the
//! active package. Committing the single active-source hop as if it reached the
//! target lands an undeclared intermediate composition stamped as the target
//! version — silently losing every later declared hop.
//!
//! # The scenario
//!
//! Active source is 1.0.0. The target 3.0.0 declares TWO deltas:
//!   "1.0.0": bridges 1.0.0 -> 2.0.0 (value += 10)
//!   "2.0.0": bridges 2.0.0 -> 3.0.0 (value += 100)
//! The spec route from 1.0.0 to 3.0.0 walks BOTH (value 0 -> 10 -> 110). Because
//! the key 2.0.0 sits strictly between 1.0.0 and 3.0.0, no connected SINGLE-HOP
//! path this runtime can execute exists, so the update is refused and value stays
//! "0". This is the sibling of `redteam_offlineage_shape_compatible_commits`: there
//! the active version had NO delta and a key sat between (off-lineage); here the
//! active version HAS a delta and a key sits between (multi-hop). Both are refused
//! by the same "no declared key strictly between" connectivity check.

mod support;

use liasse_runtime::{UpdateError, Value};
use support::{generator, load};

/// Active application 1.0.0, one counter seeded at value 0.
const ACTIVE_V1: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.twohop@1.0.0",
  "$model": {
    "counters": { "$key": "id", "id": "text", "value": "int" },
    "all": { "$view": ".counters { id, value }" }
  },
  "$data": { "counters": { "c1": { "value": "0" } } }
}"#;

/// Multi-hop target 3.0.0: TWO declared deltas, the active source 1.0.0 among them,
/// with 2.0.0 sitting strictly between 1.0.0 and 3.0.0 — a two-hop route.
const MULTIHOP_V3: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.twohop@3.0.0",
  "$model": {
    "counters": { "$key": "id", "id": "text", "value": "int" },
    "$migrations": {
      "1.0.0": [ ".counters = $old.counters { id, value: .value + 10 }" ],
      "2.0.0": [ ".counters = $old.counters { id, value: .value + 100 }" ]
    },
    "all": { "$view": ".counters { id, value }" }
  }
}"#;

/// Single-hop control 3.0.0: a LONE declared delta keyed to the active source,
/// bridging 1.0.0 directly to the package version 3.0.0 (no key strictly between).
/// This is a legitimate single hop and MUST commit (value 0 -> 10) — it proves the
/// multi-hop refusal does not over-reject a two-major single hop.
const SINGLEHOP_V3: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.twohop@3.0.0",
  "$model": {
    "counters": { "$key": "id", "id": "text", "value": "int" },
    "$migrations": {
      "1.0.0": [ ".counters = $old.counters { id, value: .value + 10 }" ]
    },
    "all": { "$view": ".counters { id, value }" }
  }
}"#;

fn value_at_head(engine: &liasse_runtime::Engine<liasse_store::MemoryStore>) -> Option<serde_json::Value> {
    let view = engine.view_at_head("all").expect("view").expect("declared");
    view.rows()[0].field("value").map(Value::to_wire)
}

#[test]
fn multi_hop_two_declared_deltas_update_is_rejected() {
    let mut engine = load("mig-twohop", ACTIVE_V1);
    let mut generator = generator();

    let result = engine.update(MULTIHOP_V3, &mut generator);

    match result {
        // §20.1/§20.3/E.9 spec-correct: the route 1.0.0 -> 3.0.0 crosses the
        // declared intermediate 2.0.0, so no connected single-hop path exists and
        // the runtime refuses rather than composing an undeclared route.
        Err(UpdateError::Rejected(_)) | Err(UpdateError::Incompatible(_)) => {}
        Err(UpdateError::Engine(other)) => {
            panic!("expected a §20.1 multi-hop rejection, got a load/engine error instead: {other}")
        }
        Ok(report) => panic!(
            "BUG (§20.1/§20.3/Annex E.9): a MULTI-HOP 3.0.0 route committed in place ({report:?}). \
             The declared key 2.0.0 sits strictly between the active 1.0.0 and the target 3.0.0, so \
             the route must compose two deltas (value 0 -> 10 -> 110); instead the engine ran only \
             `program(\"1.0.0\")` and committed the partial single hop (post-update value = {:?}), \
             silently losing the second declared hop.",
            value_at_head(&engine),
        ),
    }

    // §9.4/E.9: a refused update leaves 1.0.0 active and its seeded value intact.
    assert_eq!(
        value_at_head(&engine),
        Some(serde_json::json!("0")),
        "a refused multi-hop update must leave the 1.0.0 instance untouched (value stays 0)",
    );
}

#[test]
fn single_declared_hop_across_two_majors_commits() {
    let mut engine = load("mig-onehop", ACTIVE_V1);
    let mut generator = generator();

    engine
        .update(SINGLEHOP_V3, &mut generator)
        .expect("a lone active-source delta bridging 1.0.0 -> 3.0.0 is a single hop and must commit");

    // The single declared hop applied: value 0 + 10 = 10.
    assert_eq!(
        value_at_head(&engine),
        Some(serde_json::json!("10")),
        "the single-hop delta must have run once (value 0 -> 10)",
    );
}
