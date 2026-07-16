#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §10 addressing and exposure validation, plus the §12.1 manifest: dotted
//! addresses parse into their segments, a router refuses to bind anything the
//! model does not expose or declare, and the manifest lists exactly the granted
//! surfaces.

mod support;

use liasse_surface::{
    AddressError, Authority, CallBinding, RouterError, SurfaceAddress, SurfaceBinding,
    SurfaceRouterBuilder, ViewBinding,
};
use support::{authenticate_member, host, loaded_engine};

/// The error of a router build that must fail, or a panic (the `Ok` value is not
/// `Debug`, so this avoids `expect_err`).
fn build_error(result: Result<liasse_surface::SurfaceRouter, RouterError>) -> RouterError {
    match result {
        Err(error) => error,
        Ok(_) => panic!("expected the router build to fail"),
    }
}

#[test]
fn addresses_parse_into_authority_surface_and_call() {
    let view = SurfaceAddress::parse("public.tasks").expect("view address");
    assert_eq!(view.authority(), &Authority::Public);
    assert_eq!(view.surface(), "tasks");
    assert_eq!(view.call(), None);
    assert!(!view.is_call());

    let call = SurfaceAddress::parse("member.tasks.complete").expect("role call address");
    assert_eq!(call.role(), Some("member"));
    assert_eq!(call.surface(), "tasks");
    assert_eq!(call.call(), Some("complete"));
    assert_eq!(call.surface_prefix(), "member.tasks");
}

#[test]
fn malformed_addresses_are_rejected() {
    assert_eq!(SurfaceAddress::parse(""), Err(AddressError::EmptySegment));
    assert_eq!(SurfaceAddress::parse("public"), Err(AddressError::MissingSurface));
    assert_eq!(SurfaceAddress::parse("public.tasks.add.extra"), Err(AddressError::TooManySegments));
    assert_eq!(SurfaceAddress::parse("public..add"), Err(AddressError::EmptySegment));
}

#[test]
fn router_refuses_an_unexposed_surface() {
    let engine = loaded_engine();
    let error = build_error(
        SurfaceRouterBuilder::new()
            .public_surface("ghost", SurfaceBinding::new().with_view(ViewBinding::new("index")))
            .build(engine.model()),
    );
    assert_eq!(error, RouterError::UnexposedSurface("ghost".to_owned()));
}

#[test]
fn router_refuses_an_unexposed_call() {
    // `public.tasks` exposes add/rename/remove — binding `delete` is refused.
    let engine = loaded_engine();
    let surface = SurfaceBinding::new().with_call("delete", CallBinding::root("remove", ["id".to_owned()]));
    let error =
        build_error(SurfaceRouterBuilder::new().public_surface("tasks", surface).build(engine.model()));
    assert!(matches!(error, RouterError::UnexposedCall { ref call, .. } if call == "delete"), "{error:?}");
}

#[test]
fn router_refuses_a_binding_onto_an_undeclared_mutation() {
    let engine = loaded_engine();
    let surface = SurfaceBinding::new().with_call("add", CallBinding::root("nonexistent", ["title".to_owned()]));
    let error =
        build_error(SurfaceRouterBuilder::new().public_surface("tasks", surface).build(engine.model()));
    assert_eq!(error, RouterError::UnknownMutation("nonexistent".to_owned()));
}

#[test]
fn manifest_lists_public_and_granted_role_surfaces() {
    let mut host = host();
    host.connect("c1");
    assert!(matches!(authenticate_member(&mut host, "c1", "s_alice"), liasse_surface::AuthResult::Bound));
    let surfaces = host.manifest("c1", None).expect("manifest");
    assert!(surfaces.contains(&"public.tasks".to_owned()), "public surfaces are listed: {surfaces:?}");
    assert!(surfaces.contains(&"member.tasks".to_owned()), "the granted role surface is listed: {surfaces:?}");
}

#[test]
fn manifest_omits_role_surfaces_without_membership() {
    // With no authenticated context, only public surfaces appear.
    let mut host = host();
    host.connect("c1");
    let surfaces = host.manifest("c1", None).expect("manifest");
    assert!(surfaces.iter().all(|surface| surface.starts_with("public.")), "no role surface without a context: {surfaces:?}");
}
