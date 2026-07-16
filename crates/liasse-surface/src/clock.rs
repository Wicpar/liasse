//! The virtual clock (SPEC.md §22, Annex A.5).
//!
//! The engine samples `now()` once per admission from a [`Generators`], and the
//! surface layer judges session expiry against the same instant (§11.7). A single
//! owned [`VirtualClock`] is both: it feeds the engine as its [`Generators`] and
//! answers "what time is it" for expiry. Advancing it is how a test (or a real
//! embedding driving a scheduler) moves the observable clock forward
//! deterministically — no wall-clock, no interior mutability.

use liasse_runtime::{Generators, Precision, Timestamp};

/// A deterministic virtual clock: a fixed instant that advances only when told,
/// plus the monotone seed source behind generated identifiers (§8.12).
#[derive(Debug, Clone)]
pub struct VirtualClock {
    count: i128,
    precision: Precision,
    seed: u64,
}

impl VirtualClock {
    /// A clock at `count` ticks of `precision`, seeding identifiers from zero.
    #[must_use]
    pub fn new(count: i128, precision: Precision) -> Self {
        Self { count, precision, seed: 0 }
    }

    /// The current virtual instant.
    #[must_use]
    pub fn instant(&self) -> Timestamp {
        Timestamp::new(self.count, self.precision)
    }

    /// Move the clock forward by `ticks` of its precision (§11.7 expiry crossing,
    /// §22 clock advance). Saturating, so a test can never overflow it into a
    /// panic.
    pub fn advance(&mut self, ticks: i128) {
        self.count = self.count.saturating_add(ticks);
    }

    /// Set the clock to an absolute `count` of ticks at its precision.
    pub fn set(&mut self, count: i128) {
        self.count = count;
    }
}

impl Generators for VirtualClock {
    fn now(&mut self) -> Timestamp {
        self.instant()
    }

    fn next_seed(&mut self) -> u64 {
        let seed = self.seed;
        self.seed = self.seed.wrapping_add(1);
        seed
    }
}
