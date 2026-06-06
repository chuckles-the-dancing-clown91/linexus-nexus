//! `SeaORM` Entity for tasks table

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "tasks")]
pub struct Model {
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
    #[sea_orm(primary_key)]
    pub id: i32,
    pub task_id: Uuid,
    #[sea_orm(column_type = "Text")]
    pub intent: String,
    pub status: String,
    pub created_by: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub target_agents: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub signed_envelope: Option<String>,
    pub completed_at: Option<DateTimeWithTimeZone>,
    #[sea_orm(column_type = "Text", nullable)]
    pub error_message: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
