#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED-TEAM: the real [`SurfaceHost`] `call` path closes its §12.1 argument
//! object against the mutation's declared parameters, BUT admits ANY globally
//! registered §18.7 blob-field name as an argument to ANY mutation.
//!
//! SPEC.md §12.1: "An argument object presented to a `call` or `view` request is
//! closed: it MUST contain only names that are declared parameters of the targeted
//! mutation or view. A member whose name is not a declared parameter — including
//! any reserved `$`-prefixed name — makes the request malformed; the runtime
//! rejects it during parameter parsing (step 3), before admission, with no partial
//! effect."
//!
//! SPEC.md §18.7 step 4 binds the verified blob descriptor "to the mutation
//! parameter" — a blob parameter is a declared parameter OF THE TARGET MUTATION
//! (§10.1: "the surface parameters are the selector parameters combined with the
//! referenced mutation's parameters"; §12.1 step 5 "stream and verify blob
//! parameters"). The §18.7 exemption is therefore scoped to the TARGET mutation's
//! own declared blob parameters.
//!
//! Before the fix, `closed_call_args` exempted every name present in the host's
//! GLOBAL blob registry (`self.blobs`), keyed by blob-field name across ALL
//! mutations. So a mutation that declares NO blob parameter (`public.tasks.add`,
//! whose only declared parameter is `title`) still ADMITTED an argument named
//! after some OTHER mutation's registered blob field (`attachment`, declared only
//! by `public.intake.add`). That undeclared member was then silently dropped in
//! `build_request` and the call committed — a §12.1 violation, and it let an
//! ignored-but-present member vary the §12.3 dedup identity (`args.clone()` into
//! the `RequestModel`).
//!
//! This probe drives `SurfaceHost::call` directly, exactly as a transport binding
//! (`liasse-connect`) would.

mod support;

use liasse_host::sim::SimConnector;
use liasse_host::{Capability, ConnectorCapabilities};
use liasse_store::MemoryStore;
use liasse_surface::{
    AcceptedType, BlobEngine, BlobHost, Placement, Store, StoreId, SurfaceCall, SurfaceHost,
    SurfaceOutcome,
};
use liasse_value::MediaType;
use support::{address, args, call, host, text};

fn tasks_count(host: &SurfaceHost<MemoryStore>) -> usize {
    host.engine().view_at_head("index").expect("view").expect("declared").rows().len()
}

fn connector() -> SimConnector {
    SimConnector::new(ConnectorCapabilities::new([
        Capability::StreamUpload,
        Capability::StreamDownload,
        Capability::Checksum,
        Capability::Delete,
    ]))
}

fn blob_host() -> BlobHost<SimConnector> {
    let mut engine = BlobEngine::new();
    engine.register("fs", connector());
    engine.add_store(Store { id: StoreId::new("primary"), connector: "fs".to_owned(), enabled: true });
    let accepted = AcceptedType { max_bytes: 32, media: vec![MediaType::new("text/plain")] };
    BlobHost::new(engine, accepted, Placement::View(vec![StoreId::new("primary")]).into())
}

/// §12.1/§18.7: `attachment` is a registered blob field — but only a declared
/// parameter of `public.intake.add`. Naming it as an argument of a DIFFERENT
/// mutation (`public.tasks.add`, declaring only `title`) is malformed: the §18.7
/// blob exemption is scoped to the target mutation's OWN declared blob params, not
/// the global host registry.
#[test]
fn foreign_blob_field_name_is_rejected_on_another_mutation() {
    let mut host = host();
    host.register_blob("attachment", blob_host());
    host.connect("c1").unwrap();

    // Sanity: the declared-only call commits (one task).
    let base = host
        .call("c1", &SurfaceCall::new(address("public.tasks.add"), args([("title", text("ok"))])))
        .expect("dispatch");
    assert!(matches!(base, SurfaceOutcome::Committed { .. }), "the declared-only call commits: {base:?}");
    assert_eq!(tasks_count(&host), 1, "one task after the valid add");

    // §12.1: `attachment` is not a declared parameter of `tasks.add`. It MUST be
    // rejected as malformed even though it names a globally registered blob field.
    let outcome = host
        .call(
            "c1",
            &SurfaceCall::new(
                address("public.tasks.add"),
                args([("title", text("x")), ("attachment", text("evil"))]),
            ),
        )
        .expect("dispatch");
    assert!(
        matches!(outcome, SurfaceOutcome::Rejected(_)),
        "§12.1: `attachment` is a blob field of a DIFFERENT mutation, not a declared parameter of \
         `tasks.add`; the closed argument object MUST reject it as malformed — the §18.7 exemption \
         is target-scoped, not the global registry; got {outcome:?}",
    );
    assert_eq!(tasks_count(&host), 1, "§12.1 'no partial effect': the malformed call committed nothing");
}

/// A foreign blob field is rejected regardless of how many blob fields the host
/// has globally registered — the exemption is over the mutation's declared
/// parameter set, never any host-wide blob name.
#[test]
fn foreign_blob_field_rejected_across_multiple_registrations() {
    let mut host = host();
    host.register_blob("attachment", blob_host());
    host.register_blob("avatar", blob_host());
    host.connect("c1").unwrap();

    for field in ["attachment", "avatar"] {
        let outcome = host
            .call(
                "c1",
                &SurfaceCall::new(
                    address("public.tasks.add"),
                    args([("title", text("x")), (field, text("evil"))]),
                ),
            )
            .expect("dispatch");
        assert!(
            matches!(outcome, SurfaceOutcome::Rejected(_)),
            "§12.1: foreign blob field `{field}` is not a declared parameter of `tasks.add`; got {outcome:?}",
        );
    }
    assert_eq!(tasks_count(&host), 0, "no malformed call committed anything");
}

/// SELF-RED-TEAM (§18.7): the mutation's OWN declared blob parameter is still
/// admitted. `public.intake.add` declares the `attachment` blob param, so a
/// verified blob call binds and commits.
#[test]
fn own_declared_blob_param_is_still_admitted() {
    let mut host = host();
    host.register_blob("attachment", blob_host());
    host.connect("c1").unwrap();

    let outcome = host
        .call_with_blob(
            "c1",
            call("public.intake.add", [("title", text("with-file"))]),
            "attachment",
            b"small file",
            "text/plain",
        )
        .expect("call");
    assert!(
        matches!(outcome, SurfaceOutcome::Committed { .. }),
        "§18.7: the mutation's OWN declared blob parameter is admitted: {outcome:?}",
    );
}
