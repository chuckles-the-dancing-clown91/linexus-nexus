//! The provider operations log (and the idempotency store).

use loco_rs::prelude::*;
use sea_orm::{ActiveValue, QueryOrder, QuerySelect};
use uuid::Uuid;

pub use super::_entities::provider_operations::{self, ActiveModel, Entity, Model};

/// How long an `Idempotency-Key` replays its first answer.
pub const IDEMPOTENCY_WINDOW_HOURS: i64 = 24;

/// One operation to record.
#[derive(Debug, Clone, Default)]
pub struct NewOperation {
    pub provider: String,
    pub operation: String,
    pub target: String,
    pub requester: Option<String>,
    pub ok: bool,
    pub error: Option<String>,
    pub idempotency_key: Option<String>,
    pub response_status: Option<u16>,
    pub response: Option<String>,
}

impl Model {
    pub async fn record(db: &DatabaseConnection, op: NewOperation) -> ModelResult<Self> {
        Ok(ActiveModel {
            operation_id: ActiveValue::set(Uuid::new_v4()),
            provider: ActiveValue::set(op.provider),
            operation: ActiveValue::set(op.operation),
            target: ActiveValue::set(Some(op.target).filter(|t| !t.is_empty())),
            requester: ActiveValue::set(op.requester),
            status: ActiveValue::set(if op.ok { "ok" } else { "failed" }.to_string()),
            error: ActiveValue::set(op.error),
            idempotency_key: ActiveValue::set(op.idempotency_key),
            response_status: ActiveValue::set(op.response_status.map(i32::from)),
            response: ActiveValue::set(op.response),
            ..Default::default()
        }
        .insert(db)
        .await?)
    }

    /// The successful operation recorded under `key` within the window.
    pub async fn find_replay(db: &DatabaseConnection, key: &str) -> ModelResult<Option<Self>> {
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(IDEMPOTENCY_WINDOW_HOURS);
        let rows = Entity::find()
            .filter(provider_operations::Column::IdempotencyKey.eq(key))
            .filter(provider_operations::Column::Status.eq("ok"))
            .order_by_desc(provider_operations::Column::Id)
            .all(db)
            .await?;
        Ok(rows
            .into_iter()
            .find(|r| r.created_at.with_timezone(&chrono::Utc) >= cutoff))
    }

    /// Newest first, optionally for one provider.
    pub async fn list(
        db: &DatabaseConnection,
        provider: Option<&str>,
        limit: u64,
    ) -> ModelResult<Vec<Self>> {
        let mut q = Entity::find();
        if let Some(p) = provider {
            q = q.filter(provider_operations::Column::Provider.eq(p));
        }
        Ok(q.order_by_desc(provider_operations::Column::Id)
            .limit(limit)
            .all(db)
            .await?)
    }
}
