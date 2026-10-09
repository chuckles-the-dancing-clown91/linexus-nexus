//! `SeaORM` Entity for dns_zones table (BIND zones served by our agents)

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "dns_zones")]
pub struct Model {
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(unique)]
    pub zone_id: Uuid,
    #[sea_orm(unique)]
    pub name: String,
    pub primary_agent_id: Uuid,
    #[sea_orm(column_type = "Text", nullable)]
    pub secondary_agent_ids: Option<String>,
    pub default_ttl: i32,
    pub admin_email: String,
    pub serial: i64,
    pub last_task_id: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub task_ids: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
