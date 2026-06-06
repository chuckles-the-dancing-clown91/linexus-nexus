use loco_rs::prelude::*;
use sea_orm::ActiveValue;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use super::_entities::agents::{self, ActiveModel, Entity, Model};

#[derive(Debug, Deserialize, Serialize)]
pub struct EnrollAgentParams {
    pub hostname: String,
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
}
