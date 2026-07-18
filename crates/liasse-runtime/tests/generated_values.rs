#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! `uuid()` generated-value identity (SPEC-ISSUES #4, §5.1/§8.12).
//!
//! The pinned rule: `uuid()` yields a fresh, distinct value on every evaluation.
//! Two distinct call sites in one request differ (by span); one call site
//! evaluated across the rows of one request differs (by [`Generation`]); and the
//! derivation is a pure function of `(seed, site, generation)`, so an identical
//! triple reproduces the same value (the recording/replay guarantee — the
//! committed key is materialized once and read back, §8.12). These expectations
//! are externally deducible from the resolution, not read off the implementation.

use liasse_diag::ByteSpan;
use liasse_runtime::{derive_uuid, Generation};

fn span(start: u32, end: u32) -> ByteSpan {
    ByteSpan::new(start, end).expect("forward span")
}

#[test]
fn one_call_site_across_rows_yields_distinct_uuids() {
    // The failing case the resolution fixes: one `id: uuid = uuid()` default
    // (one fixed call site) evaluated for two rows of one request (one fixed
    // seed, an advancing generation) must NOT collide — otherwise the second
    // insert would reject on the duplicate key.
    let site = span(10, 16);
    let seed = 0xABCD_1234;
    let first = derive_uuid(seed, site, Generation::new(0));
    let second = derive_uuid(seed, site, Generation::new(1));
    assert_ne!(first, second, "the same call site must yield a distinct value per row generation");
}

#[test]
fn two_call_sites_in_one_request_do_not_collide() {
    // Two distinct `uuid()` defaults in one request (same seed, same generation,
    // different source spans) must produce different values.
    let seed = 0x0055_00AA;
    let a = derive_uuid(seed, span(4, 10), Generation::ROOT);
    let b = derive_uuid(seed, span(30, 36), Generation::ROOT);
    assert_ne!(a, b, "two distinct call sites must not share a generated value");
}

#[test]
fn identical_triple_reproduces_the_same_uuid() {
    // §8.12 recording guarantee: the derivation is pure, so an identical
    // (seed, site, generation) reproduces the recorded value verbatim.
    let seed = 0x9E37_79B9;
    let site = span(2, 8);
    let generation = Generation::new(7);
    assert_eq!(derive_uuid(seed, site, generation), derive_uuid(seed, site, generation));
}

#[test]
fn different_requests_differ_at_the_same_site_and_generation() {
    // Two admissions draw different seeds, so the same call site and generation
    // still yield different values across requests.
    let site = span(0, 6);
    let generation = Generation::new(3);
    assert_ne!(derive_uuid(1, site, generation), derive_uuid(2, site, generation));
}
