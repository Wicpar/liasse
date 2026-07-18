//! The out-of-band sources every admitted request needs: the fixed `now()`
//! sample (A.5) and the seed behind generated identifiers (§8.12).
//!
//! The engine samples both **once** at the start of an admission and holds the
//! concrete values in the request environment, so evaluation stays a pure
//! function of that environment (no interior mutability during a `&self`
//! evaluation). `uuid()` is then a pure derivation of FOUR inputs
//! (SPEC-ISSUES item 4, §5.1/§8.12): the per-request `seed` keeps two requests'
//! identifiers apart; the [`Generation`] ordinal keeps two rows of one request
//! apart, so one `uuid()` field-default call site evaluated across the several
//! rows of a single request yields a distinct value per row; and the call site —
//! its `SourceId` **and** its span — keeps two distinct `uuid()` sites of one row
//! apart. The `SourceId` is load-bearing: each field/`$key` default compiles into
//! its own sub-source, so two byte-identical `uuid()` defaults (`"uuid()"`) carry
//! the identical LOCAL span `[0..6)`; only the source that measures the span tells
//! them apart, so a `secret: uuid = uuid()` never collapses onto a public
//! `id: uuid = uuid()`. `now()` is the symmetric opposite (A.5): a single instant
//! shared by every call in the request. Replay needs no re-derivation — a
//! generated value that enters committed state is materialized at admission and
//! read back verbatim.

use liasse_diag::{ByteSpan, SourceId};
use liasse_value::{Precision, Timestamp, Uuid};

/// A per-request, per-occurrence ordinal that distinguishes each generated-value
/// evaluation (SPEC-ISSUES item 4, §5.1/§8.12). The admission hands a fresh,
/// monotonically advancing generation to each row's default resolution, so two
/// rows of one request that both default a key from the same `uuid()` call site
/// never derive the same value. It is admission bookkeeping, never committed
/// state; distinctness within one request is all it promises (replay reads the
/// recorded value, so the ordinal need not be stable across replay).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Generation(u64);

impl Generation {
    /// The generation for a context that produces at most one occurrence per
    /// call site — a view, check, computed value, or root singleton (§8.2). A
    /// per-row default resolution instead draws a fresh ordinal per row.
    pub const ROOT: Self = Generation(0);

    /// Wrap an explicit ordinal.
    #[must_use]
    pub const fn new(ordinal: u64) -> Self {
        Generation(ordinal)
    }

    /// The raw ordinal, mixed into a `uuid()` derivation.
    #[must_use]
    pub const fn ordinal(self) -> u64 {
        self.0
    }
}

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

/// Derive the UUID for a `uuid()` call in `source` at `site` under a request
/// `seed` and the current [`Generation`] (SPEC-ISSUES item 4, §5.1/§8.12). Pure:
/// identical `(seed, source, site, generation)` always yields the same UUID, and
/// a change to any one of the four yields a distinct UUID. The 128 bits are
/// filled from a SplitMix64-style avalanche of all four inputs, which spreads
/// adjacent spans, adjacent sources, sequential seeds, AND sequential generations
/// apart — so one field-default call site (one fixed `source`+`site`) evaluated
/// across the rows of a single request (one fixed `seed`, an advancing
/// `generation`) never repeats a value, and two byte-identical defaults in
/// DIFFERENT sub-sources (same local `site`, different `source`) never collide.
#[must_use]
pub fn derive_uuid(seed: u64, source: SourceId, site: ByteSpan, generation: Generation) -> Uuid {
    let ordinal = generation.ordinal();
    // The per-label sub-source index: two defaults compiled into one package
    // share a `SourceMap`, so their indices differ even when their local spans
    // are identical. Mixed into both halves so the source discriminates the site.
    let source = u64::from(source.index());
    let lo = mix(
        seed ^ u64::from(site.start()).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ source.wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
            ^ ordinal.wrapping_mul(0x2545_F491_4F6C_DD1D),
    );
    let hi = mix(
        seed.rotate_left(32)
            ^ u64::from(site.end()).wrapping_add(0xD1B5_4A32_D192_ED03)
            ^ source.rotate_left(17).wrapping_mul(0x1656_67B1_9E37_79F9)
            ^ ordinal.rotate_left(29).wrapping_mul(0xBF58_476D_1CE4_E5B9),
    );
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
