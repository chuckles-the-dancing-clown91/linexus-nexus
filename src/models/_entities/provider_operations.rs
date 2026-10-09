//! `SeaORM` Entity for provider_operations table

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "provider_operations")]
pub struct Model {
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(unique)]
    pub operation_id: Uuid,
    pub provider: String,
    pub operation: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub target: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub requester: Option<String>,
    /// `ok` or `failed`.
    pub status: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub error: Option<String>,
    pub idempotency_key: Option<String>,
    /// The HTTP status answered (replayed with the body on the same key).
    pub response_status: Option<i32>,
    #[sea_orm(column_type = "Text", nullable)]
    pub response: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
