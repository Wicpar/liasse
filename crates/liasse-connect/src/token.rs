//! Minting and checking the opaque capability tokens on the wire (§12).
//!
//! A logical connection is bound by TWO distinct values, never one:
//!
//! - a **secret credential** `C` — high-entropy, the connection bearer. It is what
//!   [`ConnKeys::connection_token`] returns, what the HttpOnly cookie /
//!   `Liasse-Connection` header carries, and the ONLY value that opens or binds a
//!   stream or authorizes a POST. It is the registry lookup key. It NEVER appears
//!   inside a frontier ([`Ft`]) or occurrence ([`Occ`]) token, so it never rides a
//!   URL, an access log, or a `Referer`.
//! - a **public id** `P` — per-connection, non-secret, distinct from `C`. It is
//!   embedded in every [`Ft`]/[`Occ`] this connection mints so the server can
//!   associate a presented token with its connection. `P` is NOT a credential:
//!   presenting a token that carries `P` grants no authority, because reaching the
//!   connection at all still requires `C`, and `P` is not `C`.
//!
//! This split is the anti-theft property (§12.2; AGENTS.md untrusted frontend). A
//! frontier token legitimately appears in the SSE `id:` and the resume URL, so it is
//! exposed to history, logs, and `Referer`; because it carries only `P`, an attacker
//! who observes one learns nothing that opens the victim's stream or POSTs as them.
//! A token that does not carry the connection's `P`, or names a position/occurrence
//! the connection never minted, is rejected as a fault rather than trusted.
//!
//! The [`TokenMinter`] is the seam D4 leaves open: the default [`UnsignedMinter`]
//! seals a payload as itself and re-opens it verbatim — correct because it is
//! `C`-gating at the registry, not `P`-secrecy, that protects the stream. An HMAC
//! minter can additionally seal the `P`+position payload with a tag without changing
//! any caller. Minting is total and never panics.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use liasse_wire::{ConnectionToken, Ft, Occ};

/// How a token payload is authenticated on the wire. The connect layer supplies
/// the payload *structure* (namespace, public id, position); a minter only seals and
/// re-opens it, so the integrity scheme is swappable without touching the registry.
pub trait TokenMinter: Send + Sync + 'static {
    /// Seal an authenticated payload into its wire form. The default is the
    /// identity — the payload carries only the non-secret public id, and the stream
    /// is protected by requiring the connection secret at the registry.
    fn seal(&self, payload: &str) -> String;

    /// Recover the payload from a presented wire token, or `None` if it fails
    /// authentication. Must never panic on hostile input.
    fn open<'t>(&self, token: &'t str) -> Option<&'t str>;

    /// A fresh high-entropy token value (a connection secret or public id).
    fn nonce(&self) -> String;
}

/// The default D4 minter: tokens are their payload verbatim. The payload carries only
/// the non-secret public id, so publishing it in a frontier `id:` leaks nothing that
/// grants authority; unforgeability of the *connection* rests on the separate secret,
/// which this minter never places on the wire. A later HMAC minter refines the tag.
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
        // connection secret or public id. A production minter would draw from a
        // CSPRNG. Hex has no `.`, so it never collides with a token's separators.
        format!("{:016x}{:016x}", self.entropy.next(), self.entropy.next())
    }
}

/// A `std`-only entropy source for the reference minter: `splitmix64` seeded from
/// wall-clock nanoseconds mixed with a process-global counter, so concurrent
/// connections never draw the same value. Not a CSPRNG — the [`TokenMinter`] seam
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

/// The two distinct secrets binding one logical connection's capability tokens (the
/// secret credential `C` and the public id `P`). Owned by the registry.
///
/// The invariant this type exists to hold: nothing derived into a wire [`Ft`]/[`Occ`]
/// (all carry only `P`) can be turned into the connection credential (`C`). `C` and
/// `P` are drawn independently, so `P` reveals nothing about `C`.
#[derive(Debug, Clone)]
pub struct ConnKeys {
    /// The secret connection credential `C`: the registry key and the bearer secret.
    /// Never embedded in any ft/occ.
    secret: String,
    /// The non-secret public id `P`: embedded in every ft/occ this connection mints,
    /// so a presented token can be associated with this connection.
    public_id: String,
}

impl ConnKeys {
    /// Bind a connection from its credential `secret` (`C`) and public id `public_id`
    /// (`P`). The two MUST be drawn independently so `P` reveals nothing about `C`.
    #[must_use]
    pub fn new(secret: String, public_id: String) -> Self {
        Self { secret, public_id }
    }

    /// The connection credential `C`: the registry lookup key and the unguessable
    /// bearer secret. It is never embedded in any ft/occ, so it never leaks through a
    /// resume URL, a log, or a `Referer`.
    #[must_use]
    pub fn connection_token(&self) -> ConnectionToken {
        ConnectionToken::new(self.secret.clone())
    }

    /// Seal a frontier position into an opaque [`Ft`], embedding the PUBLIC id `P` —
    /// never the secret. The zero-padded sequence keeps the unsigned form lexically
    /// monotone within a connection (constant `P` prefix), so an SSE `id:` stream
    /// reads in frontier order.
    #[must_use]
    pub fn frontier(&self, minter: &dyn TokenMinter, seq: u64) -> Ft {
        Ft::new(minter.seal(&format!("f.{}.{seq:020}", self.public_id)))
    }

    /// Recover the frontier position from a presented [`Ft`], confirming it carries
    /// THIS connection's public id, or `None` if it is forged, malformed, or minted
    /// for another connection. The connection is already authenticated by `C` before
    /// this runs; matching `P` only associates the token with it.
    #[must_use]
    pub fn open_frontier(&self, minter: &dyn TokenMinter, ft: &str) -> Option<u64> {
        let payload = minter.open(ft)?;
        let rest = payload.strip_prefix("f.")?;
        let (public_id, seq) = rest.split_once('.')?;
        if public_id != self.public_id {
            return None;
        }
        seq.parse::<u64>().ok()
    }

    /// Seal an occurrence counter into an opaque [`Occ`], embedding the PUBLIC id `P`.
    #[must_use]
    pub fn occurrence(&self, minter: &dyn TokenMinter, counter: u64) -> Occ {
        Occ::new(minter.seal(&format!("o.{}.{counter}", self.public_id)))
    }

    /// Recover the occurrence counter from a presented [`Occ`], confirming it carries
    /// THIS connection's public id, or `None` if it is forged, malformed, or minted
    /// for another connection. Membership in the registry index is checked
    /// separately — this only proves the token's shape and public id.
    #[must_use]
    pub fn open_occurrence(&self, minter: &dyn TokenMinter, occ: &str) -> Option<u64> {
        let payload = minter.open(occ)?;
        let rest = payload.strip_prefix("o.")?;
        let (public_id, counter) = rest.split_once('.')?;
        if public_id != self.public_id {
            return None;
        }
        counter.parse::<u64>().ok()
    }

    /// Scope a client-supplied public operation identifier to THIS connection by
    /// binding it to the connection secret `C` (§12.3, at-most-once). The result is a
    /// server-internal dedup namespace that a peer connection cannot forge: a peer
    /// only ever scopes with its OWN secret, so it can neither replay nor burn another
    /// connection's public op-id. Because `C` is stable across a §12.2 reconnect (the
    /// same connection re-presents it), an at-most-once retry still resolves. The
    /// bound value is never placed on the wire — the client keeps echoing the raw id.
    #[must_use]
    pub fn scope_operation(&self, raw: &str) -> String {
        format!("{}.{}", self.secret, raw)
    }
}
