#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! `uuid()` generated-value identity (SPEC-ISSUES #4, §5.1/§8.12).
//!
//! The pinned rule: `uuid()` yields a fresh, distinct value on every evaluation.
//! Two distinct call sites in one request differ; one call site evaluated across
//! the rows of one request differs (by [`Generation`]); and the derivation is a
//! pure function of `(seed, source, site, generation)`, so an identical tuple
//! reproduces the same value (the recording/replay guarantee — the committed key
//! is materialized once and read back, §8.12). A call site is its SOURCE plus its
//! span: each field/`$key` default compiles into its own sub-source, so two
//! byte-identical `uuid()` defaults carry the identical LOCAL span and only the
//! source that measures it tells them apart. These expectations are externally
//! deducible from the resolution, not read off the implementation.

use liasse_diag::{ByteSpan, SourceId, SourceMap};
use liasse_runtime::{derive_uuid, Generation};

fn span(start: u32, end: u32) -> ByteSpan {
    ByteSpan::new(start, end).expect("forward span")
}

/// Two distinct sub-sources standing in for two field defaults, each compiled
/// into its own label (as `compile_expr` does). Minting both from one `SourceMap`
/// gives them different indices — the discriminator the derivation must honor.
fn two_sources() -> (SourceId, SourceId) {
    let mut sources = SourceMap::new();
    let a = sources.add_label("id", "uuid()");
    let b = sources.add_label("secret", "uuid()");
    (a, b)
}

/// One sub-source, for the cases that vary only span/seed/generation.
fn one_source() -> SourceId {
    SourceMap::new().add_label("field", "uuid()")
}

#[test]
fn one_call_site_across_rows_yields_distinct_uuids() {
    // The failing case the resolution fixes: one `id: uuid = uuid()` default
    // (one fixed call site) evaluated for two rows of one request (one fixed
    // seed, an advancing generation) must NOT collide — otherwise the second
    // insert would reject on the duplicate key.
    let source = one_source();
    let site = span(10, 16);
    let seed = 0xABCD_1234;
    let first = derive_uuid(seed, source, site, Generation::new(0));
    let second = derive_uuid(seed, source, site, Generation::new(1));
    assert_ne!(first, second, "the same call site must yield a distinct value per row generation");
}

#[test]
fn two_call_sites_in_one_request_do_not_collide() {
    // Two distinct `uuid()` defaults in one request (same seed, same generation,
    // different source spans) must produce different values.
    let source = one_source();
    let seed = 0x0055_00AA;
    let a = derive_uuid(seed, source, span(4, 10), Generation::ROOT);
    let b = derive_uuid(seed, source, span(30, 36), Generation::ROOT);
    assert_ne!(a, b, "two distinct call sites must not share a generated value");
}

#[test]
fn identical_local_span_in_distinct_sources_differ() {
    // The heart of SPEC-ISSUES #4: two byte-identical `uuid()` defaults on ONE
    // row (`id`/`secret`) compile into their own sub-sources, so both carry the
    // identical LOCAL span. They share the request seed and the row's single
    // generation, so ONLY the source can tell them apart — and it must, or a
    // `secret` collapses onto the public `id`.
    let (id_source, secret_source) = two_sources();
    let seed = 0x1357_9BDF;
    let site = span(0, 6); // both defaults are the text `uuid()` → `[0..6)`.
    let id = derive_uuid(seed, id_source, site, Generation::ROOT);
    let secret = derive_uuid(seed, secret_source, site, Generation::ROOT);
    assert_ne!(
        id, secret,
        "two byte-identical defaults in distinct sub-sources must not share a value (a secret must not equal the id)"
    );
}

#[test]
fn identical_tuple_reproduces_the_same_uuid() {
    // §8.12 recording guarantee: the derivation is pure, so an identical
    // (seed, source, site, generation) reproduces the recorded value verbatim.
    let source = one_source();
    let seed = 0x9E37_79B9;
    let site = span(2, 8);
    let generation = Generation::new(7);
    assert_eq!(
        derive_uuid(seed, source, site, generation),
        derive_uuid(seed, source, site, generation)
    );
}

#[test]
fn different_requests_differ_at_the_same_site_and_generation() {
    // Two admissions draw different seeds, so the same call site and generation
    // still yield different values across requests.
    let source = one_source();
    let site = span(0, 6);
    let generation = Generation::new(3);
    assert_ne!(
        derive_uuid(1, source, site, generation),
        derive_uuid(2, source, site, generation)
    );
}
