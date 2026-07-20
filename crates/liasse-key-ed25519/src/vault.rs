//! The private-seed persistence seam for [`Ed25519KeyProvider`](crate::Ed25519KeyProvider)
//! (SPEC.md §17.6: a provider MAY "wrap local encrypted storage").
//!
//! The provider keeps live signing keys in memory; a [`KeyVault`] gives those
//! keys durability across a process restart. Two implementations ship:
//!
//! - [`EphemeralVault`] — the dev default: no persistence at all.
//! - [`EncryptedFileVault`] — an at-rest keystore directory that seals each
//!   32-byte Ed25519 seed with XChaCha20-Poly1305 under a key derived from a
//!   constructor master key. No plaintext private material is ever written.

use std::fs;
use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use sha2::{Digest, Sha512};
use zeroize::Zeroize;

/// The Ed25519 private seed length a vault seals (§17.5).
const SEED_LEN: usize = 32;
/// The XChaCha20-Poly1305 nonce length.
const NONCE_LEN: usize = 24;
/// The derived AEAD key length.
const KEY_LEN: usize = 32;

/// Where an [`Ed25519KeyProvider`](crate::Ed25519KeyProvider) persists private
/// seeds (§17.6). Seeds handed to a vault are secret; an at-rest implementation
/// MUST encrypt them before they leave process memory.
pub trait KeyVault: Send + Sync {
    /// Persist the private `seed` for `handle`, replacing any existing entry.
    ///
    /// # Errors
    /// [`VaultError`] if the seed cannot be sealed or written.
    fn store(&mut self, handle: u64, seed: &[u8; SEED_LEN]) -> Result<(), VaultError>;

    /// Forget the persisted seed for `handle` (§17.3 destroy). Removing an absent
    /// handle is not an error.
    ///
    /// # Errors
    /// [`VaultError`] if the underlying store cannot be updated.
    fn remove(&mut self, handle: u64) -> Result<(), VaultError>;

    /// Recover every persisted `(handle, seed)` pair, in ascending handle order.
    ///
    /// # Errors
    /// [`VaultError`] if the store cannot be read or an entry cannot be unsealed
    /// (e.g. a wrong master key or a tampered file).
    fn load(&self) -> Result<Vec<(u64, [u8; SEED_LEN])>, VaultError>;
}

/// A vault that persists nothing: keys live only in the provider's memory and are
/// gone at process exit. The default for
/// [`Ed25519KeyProvider::new`](crate::Ed25519KeyProvider::new).
pub struct EphemeralVault;

impl KeyVault for EphemeralVault {
    fn store(&mut self, _handle: u64, _seed: &[u8; SEED_LEN]) -> Result<(), VaultError> {
        Ok(())
    }

    fn remove(&mut self, _handle: u64) -> Result<(), VaultError> {
        Ok(())
    }

    fn load(&self) -> Result<Vec<(u64, [u8; SEED_LEN])>, VaultError> {
        Ok(Vec::new())
    }
}

/// An at-rest keystore directory whose private seeds are sealed with
/// XChaCha20-Poly1305 under a key derived from a constructor master key (§17.6).
///
/// Each key is a file `<handle>.key` holding `nonce ‖ ciphertext`; the plaintext
/// seed never touches disk. SECURITY: the sealing key is a domain-separated
/// SHA-512 of the master key, so the deployment MUST supply a high-entropy master
/// key (an OS keystore secret, a KMS-wrapped key) — this is not a
/// password-hardening KDF.
pub struct EncryptedFileVault {
    dir: PathBuf,
    cipher: XChaCha20Poly1305,
}

impl EncryptedFileVault {
    /// Open (creating if absent) the keystore `dir`, sealing seeds under a key
    /// derived from `master_key`.
    ///
    /// # Errors
    /// [`VaultError::Io`] if the directory cannot be created, or
    /// [`VaultError::Crypto`] if the derived key is rejected by the cipher.
    pub fn open(master_key: &[u8], dir: impl Into<PathBuf>) -> Result<Self, VaultError> {
        let dir = dir.into();
        fs::create_dir_all(&dir).map_err(|error| VaultError::Io(error.to_string()))?;
        let mut derived = derive_key(master_key);
        let cipher = XChaCha20Poly1305::new_from_slice(&derived)
            .map_err(|_| VaultError::Crypto("derived key has the wrong length".to_owned()))?;
        derived.zeroize();
        Ok(Self { dir, cipher })
    }

    /// The on-disk path of `handle`'s sealed seed.
    fn path(&self, handle: u64) -> PathBuf {
        self.dir.join(format!("{handle:020}.key"))
    }

    /// Seal `seed` into `nonce ‖ ciphertext`.
    fn seal(&self, seed: &[u8; SEED_LEN]) -> Result<Vec<u8>, VaultError> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|error| VaultError::Crypto(error.to_string()))?;
        let ciphertext = self
            .cipher
            .encrypt(XNonce::from_slice(&nonce), seed.as_slice())
            .map_err(|_| VaultError::Crypto("seal failed".to_owned()))?;
        let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&ciphertext);
        Ok(blob)
    }

    /// Unseal a `nonce ‖ ciphertext` blob back into a private seed.
    fn unseal(&self, blob: &[u8]) -> Result<[u8; SEED_LEN], VaultError> {
        if blob.len() < NONCE_LEN {
            return Err(VaultError::Crypto("truncated keystore entry".to_owned()));
        }
        let (nonce, ciphertext) = blob.split_at(NONCE_LEN);
        let mut plaintext = self
            .cipher
            .decrypt(XNonce::from_slice(nonce), ciphertext)
            .map_err(|_| VaultError::Crypto("unseal failed (wrong master key or tampered)".to_owned()))?;
        if plaintext.len() != SEED_LEN {
            plaintext.zeroize();
            return Err(VaultError::Crypto("keystore entry is not an Ed25519 seed".to_owned()));
        }
        let mut seed = [0u8; SEED_LEN];
        for (slot, byte) in seed.iter_mut().zip(plaintext.iter()) {
            *slot = *byte;
        }
        plaintext.zeroize();
        Ok(seed)
    }
}

impl KeyVault for EncryptedFileVault {
    fn store(&mut self, handle: u64, seed: &[u8; SEED_LEN]) -> Result<(), VaultError> {
        let blob = self.seal(seed)?;
        fs::write(self.path(handle), &blob).map_err(|error| VaultError::Io(error.to_string()))
    }

    fn remove(&mut self, handle: u64) -> Result<(), VaultError> {
        match fs::remove_file(self.path(handle)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(VaultError::Io(error.to_string())),
        }
    }

    fn load(&self) -> Result<Vec<(u64, [u8; SEED_LEN])>, VaultError> {
        let mut recovered = Vec::new();
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            // A keystore that has never been written holds no keys.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(recovered),
            Err(error) => return Err(VaultError::Io(error.to_string())),
        };
        for entry in entries {
            let path = entry.map_err(|error| VaultError::Io(error.to_string()))?.path();
            let Some(handle) = handle_of(&path) else { continue };
            let blob = fs::read(&path).map_err(|error| VaultError::Io(error.to_string()))?;
            recovered.push((handle, self.unseal(&blob)?));
        }
        recovered.sort_by_key(|(handle, _)| *handle);
        Ok(recovered)
    }
}

/// Derive the 32-byte AEAD sealing key from `master_key` with a domain-separated
/// SHA-512 (§17.6). SECURITY: not a password KDF — feed a high-entropy master key.
fn derive_key(master_key: &[u8]) -> [u8; KEY_LEN] {
    let mut hasher = Sha512::new();
    hasher.update(b"liasse-key-ed25519/v1/vault-seal");
    hasher.update(master_key);
    let digest = hasher.finalize();
    let mut key = [0u8; KEY_LEN];
    for (slot, byte) in key.iter_mut().zip(digest.iter()) {
        *slot = *byte;
    }
    key
}

/// The handle a keystore filename `<handle>.key` names, or `None` for a file that
/// is not a keystore entry.
fn handle_of(path: &Path) -> Option<u64> {
    path.file_name()?.to_str()?.strip_suffix(".key")?.parse().ok()
}

/// Why a keystore operation failed (§17.9).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VaultError {
    /// The keystore directory or a key file could not be read or written.
    #[error("keystore I/O error: {0}")]
    Io(String),
    /// A seed could not be sealed or unsealed (bad key length, wrong master key,
    /// or a tampered file).
    #[error("keystore crypto error: {0}")]
    Crypto(String),
}
