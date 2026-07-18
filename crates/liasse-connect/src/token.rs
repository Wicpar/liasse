//! Minting and checking the opaque capability tokens on the wire (§12).
//!
//! Every identity a client echoes back — the connection, a frontier
//! ([`Ft`](liasse_wire::Ft)), an occurrence ([`Occ`](liasse_wire::Occ)) — is a
//! *capability*: an opaque, high-entropy string that is meaningful only to the
//! server that minted it, revealing nothing about the internal `RowId`,
//! `CommitSeq`, or session behind it. Unforgeability rests on a per-connection
//! `nonce` no client can guess: a token that does not carry the connection's nonce,
//! or names a position/occurrence the connection never minted, is rejected as a
//! fault rather than trusted (AGENTS.md: parse hostile input at the boundary).
//!
//! The [`TokenMinter`] is the seam D4 leaves open: the default [`UnsignedMinter`]
//! authenticates a token by its nonce alone (the nonce is the secret); an HMAC
//! minter can seal the payload with a tag without changing any caller. Minting is
//! total and never panics.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use liasse_wire::{ConnectionToken, Ft, Occ};

/// How a token payload is authenticated on the wire. The connect layer supplies
/// the payload *structure* (namespace, nonce, position); a minter only seals and
/// re-opens it, so the integrity scheme is swappable without touching the registry.
pub trait TokenMinter: Send + 'static {
    /// Seal an authenticated payload into its wire form. The default is the
    /// identity — the nonce inside the payload is the capability secret.
    fn seal(&self, payload: &str) -> String;

    /// Recover the payload from a presented wire token, or `None` if it fails
    /// authentication. Must never panic on hostile input.
    fn open<'t>(&self, token: &'t str) -> Option<&'t str>;

    /// A fresh high-entropy connection nonce.
    fn nonce(&self) -> String;
}

/// The default D4 minter: tokens are their payload verbatim, authenticated by the
/// per-connection nonce they carry. Simple, deterministic in shape, and correct as
/// long as nonces are unguessable — the reference stance a later HMAC minter refines.
#[derive(Debug, Default)]
pub struct UnsignedMinter {
    entropy: Entropy,
}

impl UnsignedMinter {
    /// A minter with a fresh entropy source.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl TokenMinter for UnsignedMinter {
    fn seal(&self, payload: &str) -> String {
        payload.to_owned()
    }

    fn open<'t>(&self, token: &'t str) -> Option<&'t str> {
        Some(token)
    }

    fn nonce(&self) -> String {
        // 128 bits of entropy, hex-encoded: opaque and collision-safe for a
        // connection capability. A production minter would draw from a CSPRNG.
        format!("{:016x}{:016x}", self.entropy.next(), self.entropy.next())
    }
}

/// A `std`-only entropy source for the reference minter: `splitmix64` seeded from
/// wall-clock nanoseconds mixed with a process-global counter, so concurrent
/// connections never draw the same nonce. Not a CSPRNG — the [`TokenMinter`] seam
/// exists precisely so a deployment can supply one.
#[derive(Debug)]
struct Entropy {
    state: AtomicU64,
}

impl Default for Entropy {
    fn default() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        Self { state: AtomicU64::new(nanos ^ seq.rotate_left(32) ^ 0x9e37_79b9_7f4a_7c15) }
    }
}

impl Entropy {
    /// The next `splitmix64` draw.
    fn next(&self) -> u64 {
        let z = self.state.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed);
        let mut z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

/// A connection's minting nonce and the epoch it binds its tokens to. Owned by the
/// registry; every [`Ft`]/[`Occ`] it mints carries this nonce, and one that does
/// not is not this connection's token.
#[derive(Debug, Clone)]
pub struct Nonce(String);

impl Nonce {
    /// A connection token whose capability *is* this nonce — the lookup key and the
    /// unguessable secret in one.
    #[must_use]
    pub fn connection_token(&self) -> ConnectionToken {
        ConnectionToken::new(self.0.clone())
    }

    /// Seal a frontier position into an opaque [`Ft`]. The zero-padded sequence
    /// keeps the unsigned form lexically monotone within a connection (constant
    /// nonce prefix), so an SSE `id:` stream reads in frontier order.
    #[must_use]
    pub fn frontier(&self, minter: &dyn TokenMinter, seq: u64) -> Ft {
        Ft::new(minter.seal(&format!("f.{}.{seq:020}", self.0)))
    }

    /// Recover the frontier position from a presented [`Ft`], or `None` if it is
    /// forged, malformed, or minted for another connection.
    #[must_use]
    pub fn open_frontier(&self, minter: &dyn TokenMinter, ft: &str) -> Option<u64> {
        let payload = minter.open(ft)?;
        let rest = payload.strip_prefix("f.")?;
        let (nonce, seq) = rest.split_once('.')?;
        if nonce != self.0 {
            return None;
        }
        seq.parse::<u64>().ok()
    }

    /// Seal an occurrence counter into an opaque [`Occ`].
    #[must_use]
    pub fn occurrence(&self, minter: &dyn TokenMinter, counter: u64) -> Occ {
        Occ::new(minter.seal(&format!("o.{}.{counter}", self.0)))
    }

    /// Recover the occurrence counter from a presented [`Occ`], or `None` if it is
    /// forged, malformed, or minted for another connection. Membership in the
    /// registry index is checked separately — this only proves the token's shape and
    /// nonce.
    #[must_use]
    pub fn open_occurrence(&self, minter: &dyn TokenMinter, occ: &str) -> Option<u64> {
        let payload = minter.open(occ)?;
        let rest = payload.strip_prefix("o.")?;
        let (nonce, counter) = rest.split_once('.')?;
        if nonce != self.0 {
            return None;
        }
        counter.parse::<u64>().ok()
    }
}

impl From<String> for Nonce {
    fn from(value: String) -> Self {
        Self(value)
    }
}
