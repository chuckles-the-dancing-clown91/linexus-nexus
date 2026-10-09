//! Enrollment tokens: minted by the Hub, presented once by a new agent.

use loco_rs::prelude::*;
use sea_orm::{sea_query::Expr, ActiveValue, QueryOrder};
use uuid::Uuid;

pub use super::_entities::enrollment_tokens::{self, ActiveModel, Entity, Model};
use super::system_tokens::hash_token;

/// Plaintext prefix of an enrollment token.
pub const PREFIX: &str = "nxe_";

#[derive(Debug, Clone)]
pub struct MintParams {
    pub hostgroup: String,
    pub environment: String,
    pub label: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub ttl_minutes: i64,
    pub max_uses: i32,
    pub created_by: Option<String>,
}

/// Why a presented token cannot be used.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    Unknown,
    Expired,
    Revoked,
    UsedUp,
}

impl Model {
    /// Mint a token. Returns the row and the one-time plaintext.
    pub async fn mint(db: &DatabaseConnection, p: &MintParams) -> ModelResult<(Self, String)> {
        let plaintext = format!(
            "{PREFIX}{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        );
        let expires = chrono::Utc::now() + chrono::Duration::minutes(p.ttl_minutes);
        let row = ActiveModel {
            token_id: ActiveValue::set(Uuid::new_v4()),
            token_hash: ActiveValue::set(hash_token(&plaintext)),
            hostgroup: ActiveValue::set(p.hostgroup.clone()),
            environment: ActiveValue::set(p.environment.clone()),
            label: ActiveValue::set(p.label.clone()),
            metadata: ActiveValue::set(p.metadata.as_ref().map(ToString::to_string)),
            expires_at: ActiveValue::set(expires.into()),
            max_uses: ActiveValue::set(p.max_uses),
            uses: ActiveValue::set(0),
            agent_ids: ActiveValue::set(Some("[]".to_string())),
            revoked_at: ActiveValue::set(None),
            created_by: ActiveValue::set(p.created_by.clone()),
            ..Default::default()
        }
        .insert(db)
        .await?;
        Ok((row, plaintext))
    }

    pub async fn find_by_token_id(db: &DatabaseConnection, id: &Uuid) -> ModelResult<Self> {
        Entity::find()
            .filter(enrollment_tokens::Column::TokenId.eq(*id))
            .one(db)
            .await?
            .ok_or(ModelError::EntityNotFound)
    }

    /// Newest first.
    pub async fn list(db: &DatabaseConnection) -> ModelResult<Vec<Self>> {
        Ok(Entity::find()
            .order_by_desc(enrollment_tokens::Column::Id)
            .all(db)
            .await?)
    }

    /// Revoke (idempotent).
    pub async fn revoke(self, db: &DatabaseConnection) -> ModelResult<Self> {
        if self.revoked_at.is_some() {
            return Ok(self);
        }
        let mut active: ActiveModel = self.into();
        active.revoked_at = ActiveValue::set(Some(chrono::Utc::now().into()));
        Ok(active.update(db).await?)
    }

    /// Validate a presented plaintext without consuming it.
    pub async fn peek(
        db: &DatabaseConnection,
        plaintext: &str,
    ) -> ModelResult<Result<Self, Refusal>> {
        let plaintext = plaintext.trim();
        if !plaintext.starts_with(PREFIX) {
            return Ok(Err(Refusal::Unknown));
        }
        let Some(row) = Entity::find()
            .filter(enrollment_tokens::Column::TokenHash.eq(hash_token(plaintext)))
            .one(db)
            .await?
        else {
            return Ok(Err(Refusal::Unknown));
        };
        Ok(row.usable())
    }

    /// Whether this token can still enroll an agent.
    pub fn usable(self) -> Result<Self, Refusal> {
        if self.revoked_at.is_some() {
            Err(Refusal::Revoked)
        } else if self.expires_at.with_timezone(&chrono::Utc) <= chrono::Utc::now() {
            Err(Refusal::Expired)
        } else if self.uses >= self.max_uses {
            Err(Refusal::UsedUp)
        } else {
            Ok(self)
        }
    }

    /// Validate a presented plaintext and atomically consume one use.
    ///
    /// The use is taken with a conditional `UPDATE … SET uses = uses + 1
    /// WHERE uses < max_uses AND revoked_at IS NULL`, so two agents racing for
    /// the last use cannot both get it.
    pub async fn consume(
        db: &DatabaseConnection,
        plaintext: &str,
    ) -> ModelResult<Result<Self, Refusal>> {
        let row = match Self::peek(db, plaintext).await? {
            Ok(row) => row,
            Err(r) => return Ok(Err(r)),
        };
        let res = Entity::update_many()
            .col_expr(
                enrollment_tokens::Column::Uses,
                Expr::col(enrollment_tokens::Column::Uses).add(1),
            )
            .filter(enrollment_tokens::Column::Id.eq(row.id))
            .filter(
                Expr::col(enrollment_tokens::Column::Uses)
                    .lt(Expr::col(enrollment_tokens::Column::MaxUses)),
            )
            .filter(enrollment_tokens::Column::RevokedAt.is_null())
            .exec(db)
            .await?;
        if res.rows_affected != 1 {
            return Ok(Err(Refusal::UsedUp));
        }
        Ok(Ok(Self::find_by_token_id(db, &row.token_id).await?))
    }

    /// Record that `agent_id` enrolled with this token.
    pub async fn add_agent(self, db: &DatabaseConnection, agent_id: &Uuid) -> ModelResult<Self> {
        let mut ids = self.agent_list();
        let id = agent_id.to_string();
        if !ids.contains(&id) {
            ids.push(id);
        }
        let mut active: ActiveModel = self.into();
        active.agent_ids = ActiveValue::set(Some(serde_json::to_string(&ids).unwrap_or_default()));
        Ok(active.update(db).await?)
    }

    #[must_use]
    pub fn agent_list(&self) -> Vec<String> {
        self.agent_ids
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn metadata_value(&self) -> serde_json::Value {
        self.metadata
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| serde_json::json!({}))
    }
}
