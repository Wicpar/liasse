//! The out-of-band sources every admitted request needs: the fixed `now()`
//! sample (A.5) and the seed behind generated identifiers (§8.12).
//!
//! The engine samples both **once** at the start of an admission and holds the
//! concrete values in the request environment, so evaluation stays a pure
//! function of that environment (no interior mutability during a `&self`
//! evaluation). `uuid()` is then a pure derivation of the request seed and the
//! call site (SPEC-ISSUES item 4: per-call-site identity), so the same site in
//! one request yields one value and replay needs no re-derivation — the value is
//! already recorded in committed state.

use liasse_diag::ByteSpan;
use liasse_value::{Precision, Timestamp, Uuid};

/// Supplies the two per-request generative inputs. The engine calls each exactly
/// once per admitted request and records the results, so a conforming
/// implementation is free to draw `now()` from a real clock and `next_seed()`
/// from a CSPRNG; a test supplies deterministic values instead.
pub trait Generators {
    /// The best-effort wall-clock instant fixed for the whole request (A.5).
    fn now(&mut self) -> Timestamp;

    /// A fresh seed uniquely identifying this request's generated identifiers.
    /// Two admissions MUST draw different seeds so their `uuid()` values differ.
    fn next_seed(&mut self) -> u64;
}

/// Derive the UUID for a `uuid()` call at `site` under a request `seed`
/// (SPEC-ISSUES item 4). Pure: identical `(seed, site)` always yields the same
/// UUID, and distinct sites (or seeds) yield distinct UUIDs. The 128 bits are
/// filled from a SplitMix64-style avalanche of the seed and the site's byte
/// range, which spreads even adjacent spans and sequential seeds apart.
#[must_use]
pub fn derive_uuid(seed: u64, site: ByteSpan) -> Uuid {
    let lo = mix(seed ^ u64::from(site.start()).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let hi = mix(seed.rotate_left(32) ^ u64::from(site.end()).wrapping_add(0xD1B5_4A32_D192_ED03));
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&lo.to_be_bytes());
    bytes[8..].copy_from_slice(&hi.to_be_bytes());
    Uuid::from_bytes(bytes)
}

/// A SplitMix64 finalizer: a bijective avalanche giving good bit dispersion.
const fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A deterministic [`Generators`] that fixes `now()` at a chosen instant and
/// hands out sequential seeds. It makes admissions reproducible for tests and is
/// the least-surprising default for a single-node embedding.
#[derive(Debug, Clone)]
pub struct FixedGenerators {
    now: Timestamp,
    next: u64,
}

impl FixedGenerators {
    /// Fix `now()` at `count` ticks of `precision`, seeding identifiers from 0.
    #[must_use]
    pub fn new(count: i128, precision: Precision) -> Self {
        Self { now: Timestamp::new(count, precision), next: 0 }
    }

    /// Fix `now()` at an explicit [`Timestamp`].
    #[must_use]
    pub fn at(now: Timestamp) -> Self {
        Self { now, next: 0 }
    }
}

impl Generators for FixedGenerators {
    fn now(&mut self) -> Timestamp {
        self.now
    }

    fn next_seed(&mut self) -> u64 {
        let seed = self.next;
        self.next += 1;
        seed
    }
}
