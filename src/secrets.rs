//! Sealing secrets at rest.
//!
//! Provider API tokens are stored sealed with AES-256-GCM. The key is derived
//! from `NEXUS_SECRET_KEY` (any string; its SHA-256 is the 32-byte key). The
//! stored form is `nonce (12 bytes) ‖ ciphertext+tag`, and the provider key is
//! bound in as associated data, so a sealed DigitalOcean token cannot be
//! swapped into the Cloudflare row.
//!
//! In development only, an unset key falls back to a fixed derived key with a
//! warning. Everywhere else an unset key refuses to seal ([`SecretError::NoKey`]).

use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
    Aes256Gcm, Key, Nonce,
};
use loco_rs::environment::Environment;
use sha2::{Digest, Sha256};

/// Environment variable holding the sealing key material.
pub const ENV_SECRET_KEY: &str = "NEXUS_SECRET_KEY";
const DEV_KEY_MATERIAL: &str = "linexus-nexus-development-secret-key (do not use in production)";
const NONCE_LEN: usize = 12;

#[derive(Debug, PartialEq, Eq)]
pub enum SecretError {
    /// `NEXUS_SECRET_KEY` is unset outside development.
    NoKey,
    /// The stored value cannot be unsealed (wrong key, or corrupted).
    Unseal,
    Seal,
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoKey => write!(f, "{ENV_SECRET_KEY} is not set"),
            Self::Unseal => write!(
                f,
                "the stored credential cannot be unsealed ({ENV_SECRET_KEY} changed?)"
            ),
            Self::Seal => write!(f, "sealing failed"),
        }
    }
}

impl std::error::Error for SecretError {}

/// The sealing key for `environment`, or `NoKey`.
fn key(environment: &Environment) -> Result<Key<Aes256Gcm>, SecretError> {
    let material = match std::env::var(ENV_SECRET_KEY) {
        Ok(v) if !v.trim().is_empty() => v,
        _ if matches!(environment, Environment::Development) => {
            tracing::warn!(
                "{ENV_SECRET_KEY} is unset; sealing provider credentials with the development key"
            );
            DEV_KEY_MATERIAL.to_string()
        }
        _ => return Err(SecretError::NoKey),
    };
    let digest = Sha256::digest(material.as_bytes());
    Ok(*Key::<Aes256Gcm>::from_slice(&digest))
}

/// Whether a sealing key is available in `environment`.
#[must_use]
pub fn key_available(environment: &Environment) -> bool {
    key(environment).is_ok()
}

/// Seal `plaintext`, binding `context` (e.g. the provider key) as associated data.
pub fn seal(
    environment: &Environment,
    context: &str,
    plaintext: &str,
) -> Result<Vec<u8>, SecretError> {
    let cipher = Aes256Gcm::new(&key(environment)?);
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ct = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext.as_bytes(),
                aad: context.as_bytes(),
            },
        )
        .map_err(|_| SecretError::Seal)?;
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Open a value produced by [`seal`] with the same `context`.
pub fn open(
    environment: &Environment,
    context: &str,
    sealed: &[u8],
) -> Result<String, SecretError> {
    if sealed.len() <= NONCE_LEN {
        return Err(SecretError::Unseal);
    }
    let cipher = Aes256Gcm::new(&key(environment)?);
    let (nonce, ct) = sealed.split_at(NONCE_LEN);
    let pt = cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ct,
                aad: context.as_bytes(),
            },
        )
        .map_err(|_| SecretError::Unseal)?;
    String::from_utf8(pt).map_err(|_| SecretError::Unseal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_context_binding() {
        let env = Environment::Development;
        let sealed = seal(&env, "digitalocean", "dop_v1_secret").unwrap();
        assert!(!sealed.windows(6).any(|w| w == b"secret"));
        assert_eq!(
            open(&env, "digitalocean", &sealed).unwrap(),
            "dop_v1_secret"
        );
        assert_eq!(open(&env, "cloudflare", &sealed), Err(SecretError::Unseal));
        let mut tampered = sealed;
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert_eq!(
            open(&env, "digitalocean", &tampered),
            Err(SecretError::Unseal)
        );
    }
}
