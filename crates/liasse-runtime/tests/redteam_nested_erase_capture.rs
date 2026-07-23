#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team probe (§21.2): erasing a TOP-LEVEL row that carries a NESTED keyed
//! collection must capture that nested collection's retained payload in the
//! durable extract — the reintegration bundle — exactly as it captures the row's
//! own fields.
//!
//! §21.2: erasure "removes exactly the reachable set an ordinary deletion of the
//! same target would" and is "relocation, not destruction": everything scrubbed is
//! exported as a portable reintegration bundle (the extract), and "if the complete
//! bundle cannot be durably captured, the erasure does not commit and no bytes are
//! scrubbed, so scrubbed data is never made unrecoverable." A nested keyed
//! collection is real row state living under its parent row's identity (§5.5/§5.4):
//! removing the parent removes the whole nested subtree (`remove_subtree`), so the
//! nested rows' bytes ARE scrubbed. §21.2 step 2 therefore requires the extract to
//! capture them.
//!
//! It follows, purely from the spec, that two erasures of the same parent row that
//! differ ONLY in a nested-collection row's non-key payload MUST yield DIFFERENT
//! extracts: the bundle carries the whole removed subtree, not merely the parent's
//! own fields. If the nested payload is dropped from the bundle, both collapse to
//! the identical extract hash — the nested bytes are lost and cannot be reinserted,
//! violating "relocation, not destruction." Expectations are re-derived from §21.2,
//! not the implementation. (This mirrors the composite-key probe
//! `composite_erase_extract_captures_the_row_payload`, which pins the same property
//! for the parent row's own payload.)

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, store};

fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}

/// A `users` collection with a NESTED `orders` keyed collection carrying a
/// sensitive `secret`. `erase_user` is the §21.2 corpus shape (`return erase(row)`)
/// against the TOP-LEVEL user — a supported top-level erase whose live removal
/// takes out the whole nested subtree.
fn users_app(secret: &str) -> String {
    format!(
        r#"{{
  "$liasse": 1,
  "$app": "t.nesterase@1.0.0",
  "$model": {{
    "users": {{
      "$key": "id",
      "id": "text",
      "orders": {{ "$key": "oid", "oid": "text", "secret": "text" }}
    }},
    "all": {{ "$view": ".users {{ id, $sort: [id] }}" }},
    "$mut": {{ "erase_user": "return erase(.users[@id])" }}
  }},
  "$data": {{ "users": {{ "u1": {{ "orders": {{ "o1": {{ "secret": "{secret}" }} }} }} }} }}
}}"#
    )
}

fn user_count(engine: &Engine<MemoryStore>) -> usize {
    engine.view_at_head("all").expect("view").expect("declared").rows().len()
}

/// Erase user `u1` and return the durable extract's content-hash response text
/// (§21.2 step 6).
fn erase_user_hash(engine: &mut Engine<MemoryStore>) -> String {
    let mut g = generator();
    let outcome = engine
        .call(&CallRequest::new("erase_user").arg("id", text("u1")), &mut g)
        .expect("erase call");
    let CallOutcome::Committed { response, .. } = outcome else {
        panic!("erasing an extant user must commit a state change, got {outcome:?}");
    };
    let response = response.expect("erase returns the extract as its response");
    response.to_wire().as_str().expect("the extract response is a content-hash text").to_owned()
}

#[test]
fn nested_erase_extract_captures_the_nested_payload() {
    // Two engines, each with the IDENTICAL parent user `u1` and IDENTICAL nested
    // order key `o1`, but a DIFFERENT nested `secret`. §21.2 step 2 makes the
    // extract carry the whole removed subtree's payload, so the two extracts MUST
    // differ — the differing bytes are the nested secret.
    let mut g_a = generator();
    let mut engine_a =
        Engine::load(store("nest-erase-a"), &users_app("alpha-secret"), &mut g_a).expect("load a");
    let mut g_b = generator();
    let mut engine_b =
        Engine::load(store("nest-erase-b"), &users_app("beta-secret"), &mut g_b).expect("load b");
    assert_eq!(user_count(&engine_a), 1, "one user seeded");
    assert_eq!(user_count(&engine_b), 1, "one user seeded");

    let hash_a = erase_user_hash(&mut engine_a);
    let hash_b = erase_user_hash(&mut engine_b);

    // §21.2 step 1: the parent user (and its nested subtree) genuinely left live
    // state in both engines.
    assert_eq!(user_count(&engine_a), 0, "the erased user is gone from live state");
    assert_eq!(user_count(&engine_b), 0, "the erased user is gone from live state");

    // The extract crosses the boundary as its content hash (§21.3): non-empty and
    // never re-leaking the scrubbed nested secret.
    assert!(!hash_a.is_empty(), "the extract carries a content hash");
    assert!(!hash_b.is_empty(), "the extract carries a content hash");
    assert!(!hash_a.contains("alpha-secret"), "the scrubbed nested secret never re-leaks");
    assert!(!hash_b.contains("beta-secret"), "the scrubbed nested secret never re-leaks");

    // §21.2 step 2: the extract captures the WHOLE removed subtree, so two erasures
    // of the same parent differing only in a nested order's `secret` yield DIFFERENT
    // extracts. Equality here means the nested payload was dropped from the bundle —
    // scrubbed but unrecoverable, violating "relocation, not destruction."
    assert_ne!(
        hash_a, hash_b,
        "§21.2 step 2: the erasure extract must capture the removed row's NESTED-collection \
         payload; identical hashes mean the nested bytes were scrubbed but not captured \
         (destroyed, not relocated)",
    );
}

/// A user whose deletion CASCADES to an account (§21.1 `$on_delete: cascade`), and
/// that cascade-reached account itself carries a NESTED `ledger` collection with a
/// sensitive `secret`. `erase_user` erases the top-level user.
fn cascade_nested_app(secret: &str) -> String {
    format!(
        r#"{{
  "$liasse": 1,
  "$app": "t.cascnesterase@1.0.0",
  "$model": {{
    "users": {{ "$key": "id", "id": "text" }},
    "accounts": {{
      "$key": "id",
      "id": "text",
      "user": {{ "$ref": "/users", "$on_delete": "cascade" }},
      "ledger": {{ "$key": "eid", "eid": "text", "secret": "text" }}
    }},
    "users_all": {{ "$view": ".users {{ id, $sort: [id] }}" }},
    "accounts_all": {{ "$view": ".accounts {{ id, $sort: [id] }}" }},
    "$mut": {{ "erase_user": "return erase(.users[@id])" }}
  }},
  "$data": {{
    "users": {{ "u1": {{}} }},
    "accounts": {{ "a1": {{ "user": "u1", "ledger": {{ "e1": {{ "secret": "{secret}" }} }} }} }}
  }}
}}"#
    )
}

#[test]
fn erase_captures_nested_collection_of_a_cascade_reached_row() {
    // Erasing u1 cascades to account a1 (§21.1); a1 is in the delete-closure and
    // carries a NESTED ledger row. §21.2 scrubs the whole closure — a cascade row on
    // the same footing as the direct target — and the extract must relocate the
    // cascade row's nested ledger bytes too. Two engines differing ONLY in the nested
    // ledger secret MUST yield different extracts; equality means the cascade row's
    // nested history was scrubbed but left uncaptured.
    let mut g_a = generator();
    let mut engine_a =
        Engine::load(store("casc-a"), &cascade_nested_app("ledger-alpha"), &mut g_a).expect("load a");
    let mut g_b = generator();
    let mut engine_b =
        Engine::load(store("casc-b"), &cascade_nested_app("ledger-beta"), &mut g_b).expect("load b");

    let hash_a = erase_user_hash(&mut engine_a);
    let hash_b = erase_user_hash(&mut engine_b);

    // §21.1 delete-closure: both the direct target and the cascade account left live
    // state in both engines.
    for engine in [&engine_a, &engine_b] {
        assert_eq!(
            engine.view_at_head("users_all").expect("v").expect("d").rows().len(),
            0,
            "the erased user is gone",
        );
        assert_eq!(
            engine.view_at_head("accounts_all").expect("v").expect("d").rows().len(),
            0,
            "the cascade-reached account is gone",
        );
    }

    assert!(!hash_a.contains("ledger-alpha"), "the scrubbed nested ledger secret never re-leaks");
    assert!(!hash_b.contains("ledger-beta"), "the scrubbed nested ledger secret never re-leaks");
    assert_ne!(
        hash_a, hash_b,
        "§21.2: the erasure bundle must cover the cascade-reached row's NESTED-collection \
         payload; identical hashes mean the cascade row's nested history was scrubbed but \
         left uncaptured",
    );
}

/// A two-level nesting: users -> orders -> lines, the deepest `lines` row carrying
/// the sensitive `secret`. Erasing the top-level user must reach every level.
fn deep_nested_app(secret: &str) -> String {
    format!(
        r#"{{
  "$liasse": 1,
  "$app": "t.deepnesterase@1.0.0",
  "$model": {{
    "users": {{
      "$key": "id",
      "id": "text",
      "orders": {{
        "$key": "oid",
        "oid": "text",
        "lines": {{ "$key": "lid", "lid": "text", "secret": "text" }}
      }}
    }},
    "all": {{ "$view": ".users {{ id, $sort: [id] }}" }},
    "$mut": {{ "erase_user": "return erase(.users[@id])" }}
  }},
  "$data": {{
    "users": {{ "u1": {{ "orders": {{ "o1": {{ "lines": {{ "l1": {{ "secret": "{secret}" }} }} }} }} }} }}
  }}
}}"#
    )
}

#[test]
fn erase_captures_the_deepest_nested_level() {
    // The sensitive bytes live TWO levels down (users/orders/lines). Erasing the
    // top-level user removes the whole subtree; §21.2 requires the bundle to relocate
    // every removed level's bytes. Two engines differing only in the depth-2 secret
    // MUST yield different extracts.
    let mut g_a = generator();
    let mut engine_a =
        Engine::load(store("deep-a"), &deep_nested_app("deep-alpha"), &mut g_a).expect("load a");
    let mut g_b = generator();
    let mut engine_b =
        Engine::load(store("deep-b"), &deep_nested_app("deep-beta"), &mut g_b).expect("load b");

    let hash_a = erase_user_hash(&mut engine_a);
    let hash_b = erase_user_hash(&mut engine_b);

    assert_eq!(user_count(&engine_a), 0, "the erased user is gone");
    assert_eq!(user_count(&engine_b), 0, "the erased user is gone");
    assert!(!hash_a.contains("deep-alpha"), "the depth-2 secret never re-leaks");
    assert!(!hash_b.contains("deep-beta"), "the depth-2 secret never re-leaks");
    assert_ne!(
        hash_a, hash_b,
        "§21.2: the erasure bundle must reach the DEEPEST nested level; identical hashes \
         mean a depth-2 nested row was scrubbed but left uncaptured",
    );
}
