#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED TEAM — S5 attack battery item 1 (capability confinement / forgery) and
//! invariant #5. A forged, mutated, foreign, truncated, oversized, or non-textual
//! capability MUST resolve to a typed fault (or the defined absent/unknown path),
//! never a panic, and never another connection's rows or record.
//!
//! This whole file is now a ROBUST sign-off / regression guard: the occurrence-anchor
//! and frontier-resume paths, the operation-STATUS query, AND public operation-id call
//! idempotency all confine per connection. Two findings the red team raised are fixed
//! and guarded here and in the companion file:
//!   * The token leak (see redteam_capability_leak.rs) is closed: a ft/occ carries only
//!     the non-secret PUBLIC id, never the connection credential, so confinement no
//!     longer rests on a value the tokens publish.
//!   * FIXED (was MEDIUM): a public operation id is confined per connection
//!     (`a_public_operation_id_is_confined_per_connection`) — a peer can no longer
//!     replay or burn another connection's op-id.

mod support;

use liasse_wire::serde_json::json;
use liasse_wire::{Occ, Outcome, Upstream, WireAnchor, WireWindow};

use liasse_connect::{ConnectError, Reply};
use support::{app, call, drain, hello, server_titles, view, view_request};

/// Learn a real, live occurrence token on `conn` (a well-formed anchor).
fn live_occ(core: &mut liasse_connect::ConnectCore<liasse_store::MemoryStore>, conn: &liasse_wire::ConnectionToken) -> Occ {
    view(core, conn, "learn", "public.tasks");
    call(core, conn, "public.tasks.add", json!({ "title": "anchor" }), None);
    drain(core, conn)
        .iter()
        .filter_map(|e| liasse_wire::decode::<liasse_wire::Downstream>(&e.data).ok())
        .find_map(|frame| match frame {
            liasse_wire::Downstream::Init { rows, .. } => rows.first().map(|r| r.occ().clone()),
            liasse_wire::Downstream::Patch { ops, .. } => ops.first().map(|op| op.occ().clone()),
            _ => None,
        })
        .expect("a live occurrence token")
}

#[test]
fn a_subscriptions_occurrence_does_not_resolve_on_another_connection() {
    let mut core = app();
    let owner = hello(&mut core);
    let attacker = hello(&mut core);

    // A genuine occurrence token minted for `owner`.
    let occ = live_occ(&mut core, &owner);

    // Presented as a window anchor on `attacker`'s connection it must NOT resolve to
    // the owner's row — it carries the owner's nonce, which is not the attacker's, so
    // it is a forged token here: a typed BadToken fault, never a panic, never a row.
    let window = WireWindow { size: 2, anchor: WireAnchor::At { occ: occ.clone() }, slide: false };
    let error = core
        .submit(Some(&attacker), None, view_request("steal", "public.tasks", Some(window)))
        .expect_err("a foreign occurrence anchor is a fault");
    assert!(matches!(error, ConnectError::BadToken), "cross-connection occ is BadToken: {error:?}");

    // The SAME token on its OWN connection still resolves and opens the window — the
    // confinement is by connection, not a blanket rejection.
    let ok_window = WireWindow { size: 2, anchor: WireAnchor::At { occ }, slide: false };
    let reply = core.submit(Some(&owner), None, view_request("mine", "public.tasks", Some(ok_window))).expect("own anchor");
    assert!(matches!(reply, Reply::Opened { .. }), "the owner's own anchor opens: {reply:?}");
}

#[test]
fn adversarial_occurrence_anchors_are_faults_never_panics() {
    let mut core = app();
    let conn = hello(&mut core);
    call(&mut core, &conn, "public.tasks.add", json!({ "title": "seed" }), None);

    // Forged / mutated / truncated / oversized / non-textual occurrence tokens. Every
    // one must be a typed fault (BadToken for a forgery), never a panic, never a row.
    let hostile = [
        "",                                  // empty
        "forged-occ",                        // arbitrary
        "o.deadbeef.0",                      // right shape, wrong nonce
        "o..0",                              // empty nonce
        "o.x",                               // missing counter
        "o.x.notanumber",                    // non-numeric counter
        "o.x.99999999999999999999999999",    // counter overflows u64
        "\0",                                // embedded NUL
        "occ\u{202e}rtl",                    // unicode / RTL override
        "not/base64url+=",                   // non-base64url
        &"o.".repeat(100_000),               // oversized
    ];
    for token in hostile {
        let window = WireWindow { size: 1, anchor: WireAnchor::At { occ: Occ::new(token) }, slide: false };
        let result = core.submit(Some(&conn), None, view_request("probe", "public.tasks", Some(window)));
        // Either a typed transport fault, or a spec outcome (a well-formed-but-absent
        // anchor is a `failed` outcome). Never a leak, never a panic (reaching the
        // assert is the no-panic proof).
        match result {
            Err(ConnectError::BadToken) => {}
            Ok(Reply::Outcome(Outcome::Failed { .. })) => {}
            other => panic!("hostile anchor {token:?} produced {other:?}, not a confined fault"),
        }
    }
}

#[test]
fn a_foreign_frontier_token_cannot_replay_another_connections_stream() {
    let mut core = app();
    let owner = hello(&mut core);
    let attacker = hello(&mut core);
    view(&mut core, &owner, "w1", "public.tasks");
    view(&mut core, &attacker, "w2", "public.tasks");

    // `owner` builds up a buffered stream and a live frontier token.
    call(&mut core, &owner, "public.tasks.add", json!({ "title": "owned" }), None);
    let owner_ft = drain(&mut core, &owner)
        .iter()
        .find_map(|e| e.id.clone())
        .expect("owner frontier id");

    // The owner's frontier token is valid on the owner and invalid on the attacker —
    // it is scoped to the connection nonce.
    assert!(
        core.frontier_position(&owner, &liasse_wire::Ft::new(owner_ft.clone())).is_some(),
        "the frontier token is valid on its own connection",
    );
    assert!(
        core.frontier_position(&attacker, &liasse_wire::Ft::new(owner_ft.clone())).is_none(),
        "the frontier token does not decode on a foreign connection",
    );

    // Resuming the attacker's OWN stream with the owner's frontier token must not
    // replay the owner's buffered tail: it is unreplayable here, so the attacker gets
    // a reset + a fresh init of the attacker's OWN subscriptions.
    let events = core.resume(&attacker, Some(&owner_ft)).expect("resume tolerates a foreign id");
    let mut saw_reset = false;
    for event in &events {
        if let Ok(frame) = liasse_wire::decode::<liasse_wire::Downstream>(&event.data) {
            // The only per-subscription frame the attacker may receive names the
            // attacker's own sub `w2`, never the owner's `w1`.
            if let liasse_wire::Downstream::Init { sub, .. } = &frame {
                assert_eq!(sub.as_str(), "w2", "the attacker only re-inits its own subscription");
            }
            if matches!(frame, liasse_wire::Downstream::Reset { .. }) {
                saw_reset = true;
            }
        }
    }
    assert!(saw_reset, "a foreign frontier token is treated as an unreplayable range → reset, not a gap replay");
}

#[test]
fn a_foreign_or_forged_operation_status_query_reads_unknown() {
    let mut core = app();
    let owner = hello(&mut core);
    let attacker = hello(&mut core);

    // Owner issues an operation.
    call(&mut core, &owner, "public.tasks.add", json!({ "title": "t" }), Some("op-secret"));

    // The attacker guesses the id and queries its status: the status record is keyed
    // per connection, so the attacker learns nothing — Unknown, not the owner's
    // committed positions.
    let leaked = core
        .submit(Some(&attacker), None, Upstream::Operation { operation: liasse_wire::OperationId::new("op-secret") })
        .expect("status query");
    assert!(
        matches!(leaked, Reply::Outcome(Outcome::Unknown)),
        "a foreign operation status must not leak the owner's record: {leaked:?}",
    );

    // A structurally-garbage id is likewise Unknown, never a panic.
    for id in ["", "\0", "\u{202e}", &"x".repeat(100_000)] {
        let r = core
            .submit(Some(&attacker), None, Upstream::Operation { operation: liasse_wire::OperationId::new(id) })
            .expect("garbage status query");
        assert!(matches!(r, Reply::Outcome(Outcome::Unknown)), "garbage op id reads Unknown");
    }
}

#[test]
fn a_public_operation_id_is_confined_per_connection() {
    // REGRESSION GUARD (was FINDING MEDIUM) — a public operation id is confined per
    // connection, so it is a per-client capability (AGENTS.md), not a process-global
    // slot two anonymous connections share.
    //
    // The fix (crates/liasse-connect/src/core/live.rs `scope_operation`): a PUBLIC
    // op-id is bound to the caller's connection secret BEFORE it reaches the host's
    // §12.3 dedup log, so connection A and connection B have DISJOINT op-id
    // namespaces. A peer can neither BURN another connection's id (its distinct work
    // is not rejected) nor REPLAY another connection's retained outcome (so a
    // response-bearing public call never leaks across connections). The connector
    // always scopes with the CALLER's own secret, so a guessed or leaked id string
    // cannot select a foreign namespace.
    //
    // §12.3/§D.8 scope a public op-id by (application + target + client id); this
    // layer additionally confines it to the connection, because the connector is the
    // untrusted boundary and cannot trust a client to supply a high-entropy id. The
    // scope is stable across a §12.2 reconnect (the same connection re-presents its
    // secret) so same-connection at-most-once still holds — see `idempotent.rs`.
    // (SPEC-ISSUES residual: a genuinely high-entropy public id is deliberately NOT
    // shareable across two DISTINCT logical connections here.)
    let mut core = app();
    let a = hello(&mut core);
    let b = hello(&mut core);
    view(&mut core, &a, "wa", "public.tasks");
    view(&mut core, &b, "wb", "public.tasks");

    let first = call(&mut core, &a, "public.tasks.add", json!({ "title": "A-work" }), Some("op-shared"));
    assert!(matches!(first, Outcome::Committed { .. }), "A commits under op-shared: {first:?}");

    // B presents the SAME id string with DIFFERENT arguments. In B's own namespace it
    // is a fresh operation and COMMITS — A's record neither burns nor rejects it.
    let collide = call(&mut core, &b, "public.tasks.add", json!({ "title": "B-work" }), Some("op-shared"));
    assert!(
        matches!(collide, Outcome::Committed { .. }),
        "B's distinct op-shared is a fresh per-connection operation, not burned by A's record: {collide:?}",
    );

    // Both distinct submissions actually executed: had B replayed/collided with A's
    // record, its commit would have been a no-op re-settlement and B-work absent. B's
    // OWN subscription — swept at B's post-commit frontier (§12.3) — shows both rows.
    let titles = server_titles(&core, &b, "wb");
    assert!(titles.contains(&"A-work".to_owned()), "the shared view holds A's work: {titles:?}");
    assert!(titles.contains(&"B-work".to_owned()), "B's distinct work committed independently: {titles:?}");

    // The record B reads under `op-shared` is B's OWN ("B-work"), never A's: reusing
    // it on B with different args is B's own conflict — proving B never read or
    // inherited A's retained outcome (no cross-connection response leak).
    let echo = call(&mut core, &b, "public.tasks.add", json!({ "title": "A-work" }), Some("op-shared"));
    assert!(
        matches!(echo, Outcome::Rejected { .. }),
        "reusing B's own op-shared with new args is B's conflict — the record is B's, never A's: {echo:?}",
    );
}
