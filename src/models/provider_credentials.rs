//! Stored provider credentials (sealed) and each provider's last known state.

use loco_rs::prelude::*;
use sea_orm::ActiveValue;

pub use super::_entities::provider_credentials::{self, ActiveModel, Entity, Model};

/// The last result of testing a provider.
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub state: String,
    pub detail: String,
    pub account_id: Option<String>,
    pub account_name: Option<String>,
}

impl Model {
    pub async fn find_by_provider(
        db: &DatabaseConnection,
        provider: &str,
    ) -> ModelResult<Option<Self>> {
        Ok(Entity::find()
            .filter(provider_credentials::Column::Provider.eq(provider))
            .one(db)
            .await?)
    }

    /// Get or create the row for `provider`.
    async fn row(db: &DatabaseConnection, provider: &str) -> ModelResult<Self> {
        if let Some(row) = Self::find_by_provider(db, provider).await? {
            return Ok(row);
        }
        Ok(ActiveModel {
            provider: ActiveValue::set(provider.to_string()),
            ..Default::default()
        }
        .insert(db)
        .await?)
    }

    /// Store a sealed token (and optional account id), resetting the state.
    pub async fn store(
        db: &DatabaseConnection,
        provider: &str,
        sealed: Vec<u8>,
        account_id: Option<String>,
    ) -> ModelResult<Self> {
        let mut active: ActiveModel = Self::row(db, provider).await?.into();
        active.sealed_token = ActiveValue::set(Some(sealed));
        active.account_id = ActiveValue::set(account_id);
        active.account_name = ActiveValue::set(None);
        active.state = ActiveValue::set(Some("unknown".to_string()));
        active.detail = ActiveValue::set(None);
        active.checked_at = ActiveValue::set(None);
        Ok(active.update(db).await?)
    }

    /// Change only the account id (None clears it). The token stays; the
    /// state learnt with the old account id is forgotten, since it was about
    /// that account.
    pub async fn set_account(
        db: &DatabaseConnection,
        provider: &str,
        account_id: Option<String>,
    ) -> ModelResult<Self> {
        let mut active: ActiveModel = Self::row(db, provider).await?.into();
        active.account_id = ActiveValue::set(account_id);
        active.account_name = ActiveValue::set(None);
        active.state = ActiveValue::set(Some("unknown".to_string()));
        active.detail = ActiveValue::set(None);
        active.checked_at = ActiveValue::set(None);
        Ok(active.update(db).await?)
    }

    /// Forget the stored token and the state learnt with it.
    pub async fn clear(db: &DatabaseConnection, provider: &str) -> ModelResult<()> {
        if let Some(row) = Self::find_by_provider(db, provider).await? {
            let mut active: ActiveModel = row.into();
            active.sealed_token = ActiveValue::set(None);
            active.account_id = ActiveValue::set(None);
            active.account_name = ActiveValue::set(None);
            active.state = ActiveValue::set(None);
            active.detail = ActiveValue::set(None);
            active.checked_at = ActiveValue::set(None);
            active.update(db).await?;
        }
        Ok(())
    }

    /// Record the outcome of a test. `remember_account` also keeps the
    /// account id for later calls (stored credentials without one).
    pub async fn record_status(
        db: &DatabaseConnection,
        provider: &str,
        status: &Status,
        remember_account: bool,
    ) -> ModelResult<Self> {
        let row = Self::row(db, provider).await?;
        let keep_account = row.account_id.clone().filter(|s| !s.is_empty());
        let mut active: ActiveModel = row.into();
        active.state = ActiveValue::set(Some(status.state.clone()));
        active.detail = ActiveValue::set(Some(status.detail.clone()).filter(|d| !d.is_empty()));
        active.account_name = ActiveValue::set(status.account_name.clone());
        if remember_account || keep_account.is_none() {
            if let Some(id) = &status.account_id {
                active.account_id = ActiveValue::set(Some(id.clone()));
            }
        }
        active.checked_at = ActiveValue::set(Some(chrono::Utc::now().into()));
        Ok(active.update(db).await?)
    }
}
