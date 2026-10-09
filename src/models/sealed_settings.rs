//! Named values Nexus keeps sealed for itself (see [`crate::secrets`]).

use loco_rs::prelude::*;
use sea_orm::ActiveValue;

pub use super::_entities::sealed_settings::{self, ActiveModel, Entity, Model};

impl Model {
    pub async fn find_by_name(db: &DatabaseConnection, name: &str) -> ModelResult<Option<Self>> {
        Ok(Entity::find()
            .filter(sealed_settings::Column::Name.eq(name))
            .one(db)
            .await?)
    }

    /// Insert or replace the sealed value stored under `name`.
    pub async fn put(db: &DatabaseConnection, name: &str, sealed: Vec<u8>) -> ModelResult<Self> {
        if let Some(row) = Self::find_by_name(db, name).await? {
            let mut active: ActiveModel = row.into();
            active.sealed_value = ActiveValue::set(sealed);
            active.updated_at = ActiveValue::set(chrono::Utc::now().into());
            return Ok(active.update(db).await?);
        }
        Ok(ActiveModel {
            name: ActiveValue::set(name.to_string()),
            sealed_value: ActiveValue::set(sealed),
            ..Default::default()
        }
        .insert(db)
        .await?)
    }
}
