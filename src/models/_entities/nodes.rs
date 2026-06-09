//! `SeaORM` Entity for nodes table

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "nodes")]
pub struct Model {
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
    #[sea_orm(primary_key)]
    pub id: i32,
    pub node_id: Uuid,
    pub class: String,
    pub label: String,
    pub status: String,
    pub lifecycle_phase: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub capabilities: Option<String>,
    pub public_key: Option<String>,
    pub owner_user_id: Option<i32>,
    pub source: Option<String>,
    pub external_ref: Option<String>,
    pub commissioned_at: Option<DateTimeWithTimeZone>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
