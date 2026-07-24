use loco_rs::prelude::*;
use sea_orm::ActiveValue;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use super::_entities::agents::{self, ActiveModel, Entity, Model};

#[derive(Debug, Deserialize, Serialize)]
pub struct EnrollAgentParams {
    pub hostname: String,
    pub hostgroup: Option<String>,
    pub capability_manifest: Option<String>,
}

/// Facts an agent reports after enrolling (or on any subsequent scan). Every
/// field is optional so a partial report only touches what it carries.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct ReportFactsParams {
    pub hostname: Option<String>,
    pub hostgroup: Option<String>,
    pub os: Option<String>,
    pub kernel: Option<String>,
    pub arch: Option<String>,
    pub cpu_cores: Option<i32>,
    pub memory_mb: Option<i32>,
    pub disk_gb: Option<i32>,
    pub agent_version: Option<String>,
    pub uptime_seconds: Option<i64>,
    pub capability_manifest: Option<String>,
}

impl Model {
    /// Find agent by UUID
    pub async fn find_by_agent_id(db: &DatabaseConnection, agent_id: &Uuid) -> ModelResult<Self> {
        let agent = agents::Entity::find()
            .filter(
                model::query::condition()
                    .eq(agents::Column::AgentId, *agent_id)
                    .build(),
            )
            .one(db)
            .await?;
        agent.ok_or_else(|| ModelError::EntityNotFound)
    }

    /// Find all agents
    pub async fn find_all(db: &DatabaseConnection) -> ModelResult<Vec<Self>> {
        Ok(agents::Entity::find().all(db).await?)
    }

    /// Enroll a new agent
    pub async fn enroll(db: &DatabaseConnection, params: &EnrollAgentParams) -> ModelResult<Self> {
        let agent = agents::ActiveModel {
            agent_id: ActiveValue::set(Uuid::new_v4()),
            hostname: ActiveValue::set(params.hostname.clone()),
            status: ActiveValue::set("enrolled".to_string()),
            capability_manifest: ActiveValue::set(params.capability_manifest.clone()),
            hostgroup: ActiveValue::set(params.hostgroup.clone()),
            enrolled_at: ActiveValue::set(Some(chrono::Local::now().into())),
            last_heartbeat_at: ActiveValue::set(None),
            ..Default::default()
        }
        .insert(db)
        .await?;

        Ok(agent)
    }

    /// Record a heartbeat
    pub async fn heartbeat(self, db: &DatabaseConnection) -> ModelResult<Self> {
        let mut active: agents::ActiveModel = self.into();
        active.last_heartbeat_at = ActiveValue::set(Some(chrono::Local::now().into()));
        active.status = ActiveValue::set("healthy".to_string());
        Ok(active.update(db).await?)
    }

    /// Apply a facts report: update whatever fields are present, mark the agent
    /// healthy, and stamp the heartbeat. A report is also a liveness signal.
    pub async fn report_facts(
        self,
        db: &DatabaseConnection,
        params: &ReportFactsParams,
    ) -> ModelResult<Self> {
        let mut active: agents::ActiveModel = self.into();
        if let Some(v) = &params.hostname {
            active.hostname = ActiveValue::set(v.clone());
        }
        if params.hostgroup.is_some() {
            active.hostgroup = ActiveValue::set(params.hostgroup.clone());
        }
        if params.os.is_some() {
            active.os = ActiveValue::set(params.os.clone());
        }
        if params.kernel.is_some() {
            active.kernel = ActiveValue::set(params.kernel.clone());
        }
        if params.arch.is_some() {
            active.arch = ActiveValue::set(params.arch.clone());
        }
        if params.cpu_cores.is_some() {
            active.cpu_cores = ActiveValue::set(params.cpu_cores);
        }
        if params.memory_mb.is_some() {
            active.memory_mb = ActiveValue::set(params.memory_mb);
        }
        if params.disk_gb.is_some() {
            active.disk_gb = ActiveValue::set(params.disk_gb);
        }
        if params.agent_version.is_some() {
            active.agent_version = ActiveValue::set(params.agent_version.clone());
        }
        if params.uptime_seconds.is_some() {
            active.uptime_seconds = ActiveValue::set(params.uptime_seconds);
        }
        if params.capability_manifest.is_some() {
            active.capability_manifest = ActiveValue::set(params.capability_manifest.clone());
        }
        active.status = ActiveValue::set("healthy".to_string());
        active.last_heartbeat_at = ActiveValue::set(Some(chrono::Local::now().into()));
        Ok(active.update(db).await?)
    }
}
