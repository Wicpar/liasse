//! Order-preserving numeric primitives for the `key_enc` codec.
//!
//! Two schemes live here, both chosen so that raw byte (`memcmp`) order equals
//! the numeric order of the value they encode — the property the whole codec is
//! built on:
//!
//! - **Sign-magnitude integers** ([`write_int`], [`write_signed_i64`]) for the
//!   arbitrary-precision `int` and for a `decimal`'s exponent. A leading
//!   *sign class* byte (`neg < zero < pos`) segregates the three sign regions;
//!   a positive value is a fixed-width big-endian **length** followed by its
//!   minimal big-endian **magnitude** (longer magnitude ⇒ larger value, then
//!   magnitude bytes break ties); a negative value is the bitwise inversion of
//!   the positive body of its absolute value, which reverses the order so a
//!   larger magnitude sorts *earlier* (more negative). The length prefix makes
//!   every body self-delimiting, hence prefix-free.
//! - **Offset-binary fixed-width integers** ([`write_ob_i128`], [`write_ob_i64`],
//!   [`write_ob_i32`]) for the instant/duration/date fields whose magnitude is
//!   bounded: flipping the sign bit maps two's-complement order onto unsigned
//!   big-endian order, and the fixed width is trivially self-delimiting.

/// Sign-class prefixes shared by [`write_int`], [`write_signed_i64`], and the
/// `decimal` body: unsigned byte order is exactly `neg < zero < pos`.
pub(crate) const SIGN_NEG: u8 = 0x00;
pub(crate) const SIGN_ZERO: u8 = 0x01;
pub(crate) const SIGN_POS: u8 = 0x02;

use liasse_value::num_bigint::{BigInt, Sign};

/// Encode an arbitrary-precision integer sign-magnitude, order-preserving.
pub(crate) fn write_int(out: &mut Vec<u8>, value: &BigInt) {
    match value.sign() {
        Sign::NoSign => out.push(SIGN_ZERO),
        Sign::Plus => {
            out.push(SIGN_POS);
            write_len_mag(out, &value.magnitude().to_bytes_be());
        }
        Sign::Minus => {
            out.push(SIGN_NEG);
            let mut body = Vec::new();
            write_len_mag(&mut body, &value.magnitude().to_bytes_be());
            out.extend(body.iter().map(|byte| !byte));
        }
    }
}

/// Encode a signed 64-bit integer with the same sign-magnitude scheme — used for
/// a `decimal`'s normalized exponent.
pub(crate) fn write_signed_i64(out: &mut Vec<u8>, value: i64) {
    if value == 0 {
        out.push(SIGN_ZERO);
        return;
    }
    let magnitude = minimal_be(value.unsigned_abs());
    if value > 0 {
        out.push(SIGN_POS);
        write_len_mag(out, &magnitude);
    } else {
        out.push(SIGN_NEG);
        let mut body = Vec::new();
        write_len_mag(&mut body, &magnitude);
        out.extend(body.iter().map(|byte| !byte));
    }
}

/// A fixed-width big-endian length (4 bytes) then the minimal magnitude bytes.
///
/// The length leads so that a longer magnitude — always a larger positive value,
/// since the magnitude is minimal (no leading zero bytes) — sorts later, and the
/// prefix makes the body self-delimiting. A single key component is capped far
/// below 4 GiB, so a `u32` length never truncates in practice.
fn write_len_mag(out: &mut Vec<u8>, magnitude: &[u8]) {
    let len = u32::try_from(magnitude.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(magnitude);
}

/// The minimal big-endian bytes of a non-zero unsigned magnitude (no leading
/// zero bytes). Only called for non-zero inputs, so the result is non-empty.
fn minimal_be(value: u64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let first = bytes.iter().position(|&byte| byte != 0).unwrap_or(bytes.len());
    bytes.get(first..).unwrap_or(&[]).to_vec()
}

/// Offset-binary big-endian `i128` (16 bytes): flip the sign bit so two's
/// complement order becomes unsigned byte order.
pub(crate) fn write_ob_i128(out: &mut Vec<u8>, value: i128) {
    let bits = (value as u128) ^ (1u128 << 127);
    out.extend_from_slice(&bits.to_be_bytes());
}

/// Offset-binary big-endian `i64` (8 bytes).
pub(crate) fn write_ob_i64(out: &mut Vec<u8>, value: i64) {
    let bits = (value as u64) ^ (1u64 << 63);
    out.extend_from_slice(&bits.to_be_bytes());
}

/// Offset-binary big-endian `i32` (4 bytes).
pub(crate) fn write_ob_i32(out: &mut Vec<u8>, value: i32) {
    let bits = (value as u32) ^ (1u32 << 31);
    out.extend_from_slice(&bits.to_be_bytes());
}
