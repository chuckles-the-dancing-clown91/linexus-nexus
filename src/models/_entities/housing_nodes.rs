//! `SeaORM` Entity for housing_nodes table

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "housing_nodes")]
pub struct Model {
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
    #[sea_orm(primary_key)]
    pub id: i32,
    pub node_id: Uuid,
    pub name: String,
    pub address: String,
    pub city: Option<String>,
    pub state_province: Option<String>,
    pub postal_code: Option<String>,
    pub property_type: String,
    pub unit_count: i32,
    /// draft | pending_council | active | vacating | archived
    pub status: String,
    pub quorum_required: i32,
    #[sea_orm(column_type = "Text", nullable)]
    pub notes: Option<String>,
    pub submitted_at: Option<DateTimeWithTimeZone>,
    pub activated_at: Option<DateTimeWithTimeZone>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
