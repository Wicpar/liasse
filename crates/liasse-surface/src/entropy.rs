//! The surface admission entropy seam (SPEC.md §5.1, §8.12).
//!
//! A surface-admitted mutation may mint a generated value with `uuid()` — most
//! critically a signup/email challenge token (`email_challenges.token = uuid()`).
//! `uuid()` derives from FOUR inputs, one of which is the per-request *seed* the
//! [`Generators`] hands the admission (see [`liasse_runtime::derive_uuid`]). If
//! that seed comes from a monotone counter, the token is PREDICTABLE: an attacker
//! who observes or guesses the counter reconstructs a victim's token. So the seed
//! MUST be drawn from a cryptographically-secure RNG.
//!
//! [`Entropy`] is that source. Production defaults to [`Entropy::os`], a ChaCha
//! CSPRNG seeded from the operating-system entropy pool; a test injects a
//! deterministic, reproducible stream with [`Entropy::seeded`] through the very
//! same seam ([`SurfaceHost::with_entropy`](crate::SurfaceHost::with_entropy)),
//! exactly mirroring how the virtual clock is injectable via
//! [`SurfaceHost::new`](crate::SurfaceHost::new).
//!
//! `now()` is deliberately NOT randomized: it stays the request-fixed virtual-clock
//! instant (Annex A.5), because deterministic time is required and correct. Only
//! the randomness (`uuid()` / the request seed) is CSPRNG. The
//! [`AdmissionGenerators`] this module builds pairs the two: a fixed `now()` from
//! the clock with a `next_seed()` drawn from the CSPRNG.
//!
//! The "produced once, recorded, replayed verbatim" guarantee (§8.12) is unchanged:
//! the CSPRNG seed is drawn exactly once, at admission, when the engine evaluates
//! the request; the generated value it derives enters committed state and is read
//! back verbatim. Dedup replay (§12.3) and restart (§22) never re-admit, so they
//! never re-draw — the recorded token is reused as-is.

use liasse_runtime::{Generators, Timestamp};
use rand_chacha::ChaCha12Rng;
use rand_core::{RngCore, SeedableRng};

/// The source of per-request admission seeds (§5.1, §8.12). Production is a ChaCha
/// CSPRNG seeded from the operating system, so a surface-minted `uuid()` token is
/// unpredictable; a test injects either a seeded CSPRNG or a deterministic
/// sequential source for reproducibility. It is plain owned state advanced by
/// `&mut`, with no interior mutability — the surface host owns exactly one and
/// threads it into each admission.
///
/// [`Clone`] snapshots the stream position so a driver that rebuilds a host for a
/// §22 volatile restart can carry the source forward — keeping the post-restart
/// admissions on a fresh, non-colliding continuation of the same sequence rather
/// than replaying the pre-restart seeds (a determinism concern for a reproducible
/// harness; a production `os()` host restarts on fresh OS entropy instead).
#[derive(Clone)]
pub struct Entropy {
    source: Source,
}

/// The two admission-seed backends behind [`Entropy`].
///
/// The CSPRNG state dwarfs the counter, but there is exactly ONE [`Entropy`] per
/// host and the production variant is always the large [`Csprng`](Source::Csprng),
/// so boxing it would only add a pointless heap indirection on the seed-draw path
/// for the singleton; the size skew is deliberately accepted.
#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
enum Source {
    /// A ChaCha CSPRNG — the production default and the seeded test source.
    Csprng(ChaCha12Rng),
    /// A monotone counter reproducing the legacy generator sequence. For
    /// DETERMINISTIC CONFORMANCE HARNESSES ONLY (see [`Entropy::sequential`]);
    /// never a production source, as its seeds are trivially predictable.
    Sequential(u64),
}

impl Entropy {
    /// The production default: a ChaCha CSPRNG seeded from the operating-system
    /// entropy source. The seed is drawn once here; every subsequent seed is a
    /// pure CSPRNG output, so a surface-minted `uuid()` token is unpredictable and
    /// the hot admission path performs no further syscall.
    ///
    /// # Panics
    /// If the operating-system CSPRNG cannot supply the initial seed. A token
    /// minted from a guessable seed reintroduces the very defect this seam closes,
    /// so an unavailable entropy source is an unrecoverable state (AGENTS.md's
    /// no-panic exception) — the host refuses to run rather than mint predictable
    /// secrets. This mirrors the standard library `OsRng` contract the seam stands
    /// in for; on any running host the OS pool is available and this never fires.
    #[must_use]
    pub fn os() -> Self {
        let mut seed = <ChaCha12Rng as SeedableRng>::Seed::default();
        // SECURITY (§5.1/§8.12): the initial seed of the surface admission CSPRNG
        // MUST be real OS entropy; there is no safe fallback, so a failure here is
        // fatal by design rather than silently downgraded to a guessable value.
        #[allow(clippy::expect_used)]
        getrandom::fill(&mut seed).expect("operating-system CSPRNG must seed surface admission entropy");
        Self { source: Source::Csprng(ChaCha12Rng::from_seed(seed)) }
    }

    /// A deterministic, reproducible CSPRNG seeded from `seed` — for tests that
    /// need admission to replay identically while still exercising the CSPRNG path.
    /// Two `Entropy::seeded(k)` over the same package produce the same
    /// generated-value sequence, so a test can assert reproducibility while
    /// production stays on [`Entropy::os`].
    #[must_use]
    pub fn seeded(seed: u64) -> Self {
        Self { source: Source::Csprng(ChaCha12Rng::seed_from_u64(seed)) }
    }

    /// A deterministic monotone seed source starting at `start`, reproducing the
    /// legacy counter generation.
    ///
    /// FOR DETERMINISTIC CONFORMANCE HARNESSES ONLY — never production. The
    /// file-based corpus matches generated `uuid()` values positionally and through
    /// `$bind`/`$ref`, and its expectations were authored against the monotone
    /// generator, so the testkit adapter pins admission to this source to keep the
    /// corpus reproducible run-to-run. Because the seeds are `start, start+1, …`
    /// they are trivially predictable — the very defect [`os`](Self::os) closes —
    /// so a real deployment must never construct this.
    #[must_use]
    pub fn sequential(start: u64) -> Self {
        Self { source: Source::Sequential(start) }
    }

    /// Draw the next per-request seed for a surface admission (§8.12).
    pub(crate) fn next_seed(&mut self) -> u64 {
        match &mut self.source {
            Source::Csprng(rng) => rng.next_u64(),
            Source::Sequential(counter) => {
                let seed = *counter;
                *counter = counter.wrapping_add(1);
                seed
            }
        }
    }

    /// Build the [`Generators`] one surface admission runs under: `now` is the
    /// request-fixed virtual-clock instant (Annex A.5), while `next_seed()` draws
    /// from this CSPRNG (§5.1/§8.12).
    pub(crate) fn generators(&mut self, now: Timestamp) -> AdmissionGenerators<'_> {
        AdmissionGenerators { now, entropy: self }
    }
}

impl Default for Entropy {
    fn default() -> Self {
        Self::os()
    }
}

/// The [`Generators`] a single surface-admitted mutation is admitted under. It
/// separates the two out-of-band inputs by their required nature: `now()` is the
/// deterministic, request-fixed clock instant (Annex A.5), while `next_seed()` is
/// a fresh CSPRNG draw (§5.1/§8.12) so `uuid()` is unpredictable. Borrows the
/// host's [`Entropy`] for the duration of the admission and nothing longer.
pub(crate) struct AdmissionGenerators<'a> {
    now: Timestamp,
    entropy: &'a mut Entropy,
}

impl Generators for AdmissionGenerators<'_> {
    fn now(&mut self) -> Timestamp {
        self.now
    }

    fn next_seed(&mut self) -> u64 {
        self.entropy.next_seed()
    }
}
