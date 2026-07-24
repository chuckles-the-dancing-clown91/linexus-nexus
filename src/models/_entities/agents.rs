//! `SeaORM` Entity for agents table

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "agents")]
pub struct Model {
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
    #[sea_orm(primary_key)]
    pub id: i32,
    pub agent_id: Uuid,
    pub hostname: String,
    pub status: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub capability_manifest: Option<String>,
    pub enrolled_at: Option<DateTimeWithTimeZone>,
    pub last_heartbeat_at: Option<DateTimeWithTimeZone>,
    // Facts reported by the agent's registry scan, surfaced by Daedalus IT on
    // the machine profile. All nullable — an agent may enroll before reporting.
    #[sea_orm(column_type = "Text", nullable)]
    pub hostgroup: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub os: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub kernel: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub arch: Option<String>,
    pub cpu_cores: Option<i32>,
    pub memory_mb: Option<i32>,
    pub disk_gb: Option<i32>,
    #[sea_orm(column_type = "Text", nullable)]
    pub agent_version: Option<String>,
    pub uptime_seconds: Option<i64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
