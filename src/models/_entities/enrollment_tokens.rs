//! `SeaORM` Entity for enrollment_tokens table

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "enrollment_tokens")]
pub struct Model {
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(unique)]
    pub token_id: Uuid,
    #[sea_orm(unique)]
    pub token_hash: String,
    pub hostgroup: String,
    pub environment: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub label: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub metadata: Option<String>,
    pub expires_at: DateTimeWithTimeZone,
    pub max_uses: i32,
    pub uses: i32,
    #[sea_orm(column_type = "Text", nullable)]
    pub agent_ids: Option<String>,
    pub revoked_at: Option<DateTimeWithTimeZone>,
    pub created_by: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
