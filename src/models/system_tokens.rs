//! Service-to-service credentials.
//!
//! Tokens are random opaque strings. Only their SHA-256 hash is persisted; the
//! plaintext is returned exactly once, at issuance. Verification hashes the
//! presented token and looks for an active match. Scopes gate what a service may
//! do — `"*"` grants all, otherwise an exact or `prefix:*` match is required.

use loco_rs::prelude::*;
use sea_orm::ActiveValue;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub use super::_entities::system_tokens::{self, ActiveModel, Entity, Model};

/// SHA-256 hex digest of a token's plaintext.
#[must_use]
pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

impl Model {
    /// Look up an active token by the hash of its plaintext.
    pub async fn find_active_by_hash(
        db: &DatabaseConnection,
        hash: &str,
    ) -> ModelResult<Option<Self>> {
        Ok(Entity::find()
            .filter(
                model::query::condition()
                    .eq(system_tokens::Column::TokenHash, hash)
                    .eq(system_tokens::Column::Active, true)
                    .build(),
            )
            .one(db)
            .await?)
    }

    pub async fn find_by_service(
        db: &DatabaseConnection,
        service: &str,
    ) -> ModelResult<Option<Self>> {
        Ok(Entity::find()
            .filter(
                model::query::condition()
                    .eq(system_tokens::Column::Service, service)
                    .build(),
            )
            .one(db)
            .await?)
    }

    /// Issue a new token for a service. Returns the persisted record and the
    /// one-time plaintext (never stored, never recoverable).
    pub async fn issue(
        db: &DatabaseConnection,
        service: &str,
        scopes: &str,
    ) -> ModelResult<(Self, String)> {
        let plaintext = format!("nx_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let row = system_tokens::ActiveModel {
            token_id: ActiveValue::set(Uuid::new_v4()),
            service: ActiveValue::set(service.to_string()),
            token_hash: ActiveValue::set(hash_token(&plaintext)),
            scopes: ActiveValue::set(Some(scopes.to_string())),
            active: ActiveValue::set(true),
            ..Default::default()
        }
        .insert(db)
        .await?;
        Ok((row, plaintext))
    }

    /// Record that the token was just used.
    pub async fn touch(self, db: &DatabaseConnection) -> ModelResult<Self> {
        let mut active: system_tokens::ActiveModel = self.into();
        active.last_used_at = ActiveValue::set(Some(chrono::Local::now().into()));
        Ok(active.update(db).await?)
    }

    /// Parsed scope grants.
    #[must_use]
    pub fn scope_list(&self) -> Vec<String> {
        self.scopes
            .as_deref()
            .unwrap_or("")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Whether this token may perform `scope`. `"*"` grants all; a stored
    /// `prefix:*` grants any scope under that prefix.
    #[must_use]
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scope_list().iter().any(|s| {
            s == "*"
                || s == scope
                || (s.ends_with('*') && scope.starts_with(s.trim_end_matches('*')))
        })
    }
}
