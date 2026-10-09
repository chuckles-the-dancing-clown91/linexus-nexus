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
    // Policy, not facts. Everything above is what the agent found out about
    // itself; these are what the Hub decided about it and pushed down. They
    // live here because Nexus is the inventory authority — a reinstalled
    // agent enrolls and learns what it used to be, instead of coming back as
    // an anonymous production node that starts paging somebody.
    #[sea_orm(column_type = "Text")]
    pub environment: String,
    pub monitored: bool,
    #[sea_orm(column_type = "Text", nullable)]
    pub monitor_note: Option<String>,
    pub environment_updated_at: Option<DateTimeWithTimeZone>,
    // Identity. `machine_id` re-adopts a re-enrolling machine; the agent's own
    // `nxa_` credential is kept only as its SHA-256.
    pub machine_id: Option<String>,
    pub credential_hash: Option<String>,
    pub enrollment_token_id: Option<Uuid>,
    /// The Hub's ids carried on the enrollment token (JSON object).
    #[sea_orm(column_type = "Text", nullable)]
    pub metadata: Option<String>,
    // Richer facts (JSON text), stamped by `facts_at`.
    pub public_ip: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub interfaces: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub listening: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub services: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub packages: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub dns_server: Option<String>,
    pub facts_at: Option<DateTimeWithTimeZone>,
    /// The last `install_dns_server` task dispatched to this agent.
    pub dns_install_task_id: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
