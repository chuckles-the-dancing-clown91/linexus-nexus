//! Signed plans (`docs/PROVIDERS.md` §12).
//!
//! Every task handed to an agent carries an Ed25519 `envelope` over a small
//! JSON payload (`v`, `taskId`, `agentId`, `intent`, `plan`, `issuedAt`,
//! `expiresAt`, `nonce`). The agent verifies the signature over the exact
//! payload bytes with the public key it pinned at install / enrollment, checks
//! `agentId`, `expiresAt` and replay by `taskId`, and executes only
//! `payload.plan` — so a compromised network path or a proxy cannot hand it
//! work Nexus did not issue.
//!
//! The key is the 32-byte seed in `NEXUS_SIGNING_KEY` (standard, padded
//! base64). Without it Nexus generates a seed on first start and keeps it
//! sealed under `NEXUS_SECRET_KEY` in `sealed_settings`, reusing it on later
//! starts; when the variable is set and differs from what is stored, the
//! variable wins and replaces the stored seed (rotation). `keyId` is the
//! first 16 hex characters of SHA-256 over the 32-byte public key.

use std::sync::Arc;

use aes_gcm::aead::{rand_core::RngCore, OsRng};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use chrono::{DateTime, SecondsFormat, Utc};
use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};
use loco_rs::{app::AppContext, environment::Environment};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::models::sealed_settings;
use crate::secrets::{self, SecretError};

/// Environment variable holding the base64 Ed25519 seed.
pub const ENV_SIGNING_KEY: &str = "NEXUS_SIGNING_KEY";
/// Environment variable holding a signed plan's lifetime in seconds.
pub const ENV_PLAN_TTL: &str = "NEXUS_PLAN_TTL_SECS";
/// Short on purpose: envelopes are re-signed on every poll, so the TTL only
/// bounds delivery → execution, and the agent's replay memory (its last
/// 1000 executed task ids) must outlast it.
pub const DEFAULT_PLAN_TTL_SECS: i64 = 3_600;
pub const ALG: &str = "ed25519";
/// Payload format version.
pub const PAYLOAD_VERSION: i64 = 1;
/// `sealed_settings` row (and sealing context) holding the generated seed.
const SETTING: &str = "signing_key.ed25519";

/// The plan signing key. Cheap to clone.
#[derive(Clone)]
pub struct Signer {
    key: Arc<SigningKey>,
    key_id: String,
}

impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signer")
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

/// `keyId` of a public key: the first 16 hex characters of its SHA-256.
#[must_use]
pub fn key_id(public: &VerifyingKey) -> String {
    hex::encode(Sha256::digest(public.as_bytes()))[..16].to_string()
}

/// Decode a base64 (standard, padded) 32-byte seed.
pub fn parse_seed(raw: &str) -> Result<[u8; 32], String> {
    let bytes = B64
        .decode(raw.trim())
        .map_err(|_| format!("{ENV_SIGNING_KEY} is not valid base64"))?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
        format!(
            "{ENV_SIGNING_KEY} must be 32 bytes (an Ed25519 seed), got {}",
            bytes.len()
        )
    })
}

impl Signer {
    #[must_use]
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        let key = SigningKey::from_bytes(seed);
        let key_id = key_id(&key.verifying_key());
        Self {
            key: Arc::new(key),
            key_id,
        }
    }

    /// A fresh random key.
    #[must_use]
    pub fn generate() -> Self {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        Self::from_seed(&seed)
    }

    fn seed(&self) -> [u8; 32] {
        self.key.to_bytes()
    }

    #[must_use]
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    #[must_use]
    pub fn verifying_key(&self) -> VerifyingKey {
        self.key.verifying_key()
    }

    /// The public key, base64 (standard, padded).
    #[must_use]
    pub fn public_key_b64(&self) -> String {
        B64.encode(self.key.verifying_key().as_bytes())
    }

    /// `{alg, keyId, publicKey}` — served by `GET /api/v1/signing-key` and in
    /// the enrollment answer.
    #[must_use]
    pub fn public_json(&self) -> Value {
        json!({
            "alg": ALG,
            "keyId": self.key_id,
            "publicKey": self.public_key_b64(),
        })
    }

    /// `{alg, keyId, payload, signature}` over the exact `payload` bytes.
    #[must_use]
    pub fn envelope(&self, payload: &[u8]) -> Value {
        let signature = self.key.sign(payload);
        json!({
            "alg": ALG,
            "keyId": self.key_id,
            "payload": B64.encode(payload),
            "signature": B64.encode(signature.to_bytes()),
        })
    }
}

/// What one task's signed payload says.
#[derive(Debug, Clone)]
pub struct PlanPayload<'a> {
    pub task_id: &'a str,
    pub agent_id: &'a str,
    pub intent: &'a str,
    pub plan: &'a Value,
    pub issued_at: DateTime<Utc>,
    pub ttl_secs: i64,
}

impl PlanPayload<'_> {
    /// The payload bytes: a UTF-8 JSON object with exactly `v`, `taskId`,
    /// `agentId`, `intent`, `plan`, `issuedAt`, `expiresAt` and `nonce` (16
    /// random bytes, hex). Times are RFC 3339 UTC.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut nonce = [0u8; 16];
        OsRng.fill_bytes(&mut nonce);
        let expires_at = self.issued_at + chrono::Duration::seconds(self.ttl_secs);
        json!({
            "v": PAYLOAD_VERSION,
            "taskId": self.task_id,
            "agentId": self.agent_id,
            "intent": self.intent,
            "plan": self.plan,
            "issuedAt": self.issued_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            "expiresAt": expires_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            "nonce": hex::encode(nonce),
        })
        .to_string()
        .into_bytes()
    }
}

/// A signed plan's lifetime: `NEXUS_PLAN_TTL_SECS`, default 1 h (an empty
/// or invalid value counts as unset).
#[must_use]
pub fn plan_ttl_secs() -> i64 {
    std::env::var(ENV_PLAN_TTL)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_PLAN_TTL_SECS)
}

/// Whether `environment` is a development or test one (where generating
/// keys quietly is fine).
fn relaxed(environment: &Environment) -> bool {
    matches!(environment, Environment::Development | Environment::Test)
}

/// The stored seed: `Ok(None)` when there is none.
async fn stored_seed(ctx: &AppContext) -> Result<Option<[u8; 32]>, String> {
    let row = sealed_settings::Model::find_by_name(&ctx.db, SETTING)
        .await
        .map_err(|e| format!("reading the stored signing key: {e}"))?;
    let Some(row) = row else { return Ok(None) };
    let text =
        secrets::open(&ctx.environment, SETTING, &row.sealed_value).map_err(|e| match e {
            SecretError::NoKey => format!(
                "a signing key is stored but {} is not set to unseal it",
                secrets::ENV_SECRET_KEY
            ),
            _ => format!(
                "the stored signing key cannot be unsealed ({} changed?)",
                secrets::ENV_SECRET_KEY
            ),
        })?;
    parse_seed(&text)
        .map(Some)
        .map_err(|_| "the stored signing key is corrupt".to_string())
}

/// Seal and store `signer`'s seed. `false` (with a warning) when no sealing
/// key is available — the key then lasts only as long as this process.
async fn persist(ctx: &AppContext, signer: &Signer) -> Result<bool, String> {
    let sealed = match secrets::seal(&ctx.environment, SETTING, &B64.encode(signer.seed())) {
        Ok(s) => s,
        Err(SecretError::NoKey) => {
            tracing::warn!(
                "{} is unset: the plan signing key is not persisted and changes on every start",
                secrets::ENV_SECRET_KEY
            );
            return Ok(false);
        }
        Err(e) => return Err(format!("sealing the signing key: {e}")),
    };
    sealed_settings::Model::put(&ctx.db, SETTING, sealed)
        .await
        .map_err(|e| format!("storing the signing key: {e}"))?;
    Ok(true)
}

/// Resolve the signing key at boot (see the module docs) and keep it in the
/// shared store. Errors are reasons to refuse to start.
pub async fn init(ctx: &AppContext) -> Result<Signer, String> {
    let from_env = match std::env::var(ENV_SIGNING_KEY) {
        Ok(v) if !v.trim().is_empty() => Some(Signer::from_seed(&parse_seed(&v)?)),
        _ => None,
    };
    let stored = stored_seed(ctx).await;
    let signer = match (from_env, stored) {
        (Some(env), Ok(Some(seed))) if seed == env.seed() => env,
        (Some(env), stored) => {
            if matches!(stored, Ok(Some(_)) | Err(_)) {
                tracing::info!(key_id = %env.key_id(), "{ENV_SIGNING_KEY} replaces the stored plan signing key");
            }
            persist(ctx, &env).await?;
            env
        }
        (None, Ok(Some(seed))) => Signer::from_seed(&seed),
        (None, Ok(None)) => {
            let fresh = Signer::generate();
            let kept = persist(ctx, &fresh).await?;
            if relaxed(&ctx.environment) {
                tracing::info!(key_id = %fresh.key_id(), persisted = kept, "generated a plan signing key");
            } else {
                tracing::warn!(
                    key_id = %fresh.key_id(),
                    "!!! {ENV_SIGNING_KEY} is unset: generated a NEW plan signing key and stored it sealed. \
                     Agents pin this key; back it up (or set {ENV_SIGNING_KEY}) so it survives a lost database. !!!"
                );
            }
            fresh
        }
        (None, Err(why)) if relaxed(&ctx.environment) => {
            tracing::warn!("{why}; generating a new plan signing key (agents that pinned the old one must be re-pinned)");
            let fresh = Signer::generate();
            persist(ctx, &fresh).await?;
            fresh
        }
        (None, Err(why)) => {
            return Err(format!(
                "{why}; set {ENV_SIGNING_KEY} or restore {}",
                secrets::ENV_SECRET_KEY
            ))
        }
    };
    ctx.shared_store.insert(signer.clone());
    Ok(signer)
}

/// The signing key resolved at boot (resolving it now if boot did not).
pub async fn signer(ctx: &AppContext) -> Result<Signer, String> {
    if let Some(s) = ctx.shared_store.get::<Signer>() {
        return Ok(s);
    }
    init(ctx).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, Verifier};

    #[test]
    fn envelope_verifies_over_the_payload_bytes() {
        let signer = Signer::from_seed(&[7u8; 32]);
        let plan = json!({"steps": [{"id": "s1", "action": "command.run"}]});
        let payload = PlanPayload {
            task_id: "t-1",
            agent_id: "a-1",
            intent: "run_command",
            plan: &plan,
            issued_at: DateTime::parse_from_rfc3339("2026-10-09T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            ttl_secs: 3600,
        }
        .to_bytes();
        let env = signer.envelope(&payload);
        assert_eq!(env["alg"], "ed25519");
        assert_eq!(env["keyId"], signer.key_id());

        let bytes = B64.decode(env["payload"].as_str().unwrap()).unwrap();
        assert_eq!(bytes, payload);
        let sig = B64.decode(env["signature"].as_str().unwrap()).unwrap();
        let sig = Signature::from_bytes(&<[u8; 64]>::try_from(sig.as_slice()).unwrap());
        let public = B64.decode(signer.public_key_b64()).unwrap();
        let public =
            VerifyingKey::from_bytes(&<[u8; 32]>::try_from(public.as_slice()).unwrap()).unwrap();
        assert!(public.verify(&bytes, &sig).is_ok());
        let mut tampered = bytes.clone();
        tampered[10] ^= 1;
        assert!(public.verify(&tampered, &sig).is_err());

        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "agentId",
                "expiresAt",
                "intent",
                "issuedAt",
                "nonce",
                "plan",
                "taskId",
                "v"
            ]
        );
        assert_eq!(v["v"], 1);
        assert_eq!(v["plan"], plan);
        assert_eq!(v["issuedAt"], "2026-10-09T12:00:00Z");
        assert_eq!(v["expiresAt"], "2026-10-09T13:00:00Z");
        assert_eq!(v["nonce"].as_str().unwrap().len(), 32);
    }

    #[test]
    fn key_id_and_seed_parsing() {
        let signer = Signer::from_seed(&[1u8; 32]);
        let expected = hex::encode(Sha256::digest(signer.verifying_key().as_bytes()));
        assert_eq!(signer.key_id(), &expected[..16]);
        assert_eq!(parse_seed(&B64.encode([1u8; 32])).unwrap(), [1u8; 32]);
        assert!(parse_seed("not base64!").is_err());
        assert!(parse_seed(&B64.encode([1u8; 31])).is_err());
    }
}
