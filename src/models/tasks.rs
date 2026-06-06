use loco_rs::prelude::*;
use sea_orm::ActiveValue;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use super::_entities::tasks::{self, ActiveModel, Entity, Model};

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateTaskParams {
    pub intent: String,
    pub target_agents: Option<Vec<String>>,
}

impl Model {
    /// Find task by UUID
    pub async fn find_by_task_id(db: &DatabaseConnection, task_id: &Uuid) -> ModelResult<Self> {
        let task = tasks::Entity::find()
            .filter(
                model::query::condition()
                    .eq(tasks::Column::TaskId, *task_id)
                    .build(),
            )
            .one(db)
            .await?;
        task.ok_or_else(|| ModelError::EntityNotFound)
    }

    /// Find all tasks
    pub async fn find_all(db: &DatabaseConnection) -> ModelResult<Vec<Self>> {
        Ok(tasks::Entity::find().all(db).await?)
    }

    /// Create a new task
    pub async fn create(
        db: &DatabaseConnection,
        created_by: &str,
        params: &CreateTaskParams,
    ) -> ModelResult<Self> {
        let target_agents_json = params
            .target_agents
            .as_ref()
            .map(|agents| serde_json::to_string(agents).unwrap_or_default());

        let task = tasks::ActiveModel {
            task_id: ActiveValue::set(Uuid::new_v4()),
            intent: ActiveValue::set(params.intent.clone()),
            status: ActiveValue::set("pending".to_string()),
            created_by: ActiveValue::set(created_by.to_string()),
            target_agents: ActiveValue::set(target_agents_json),
            signed_envelope: ActiveValue::set(None),
            completed_at: ActiveValue::set(None),
            error_message: ActiveValue::set(None),
            ..Default::default()
        }
        .insert(db)
        .await?;

        Ok(task)
    }

    /// Update task status
    pub async fn update_status(self, db: &DatabaseConnection, status: &str) -> ModelResult<Self> {
        let mut active: tasks::ActiveModel = self.into();
        active.status = ActiveValue::set(status.to_string());
        if status == "completed" || status == "failed" {
            active.completed_at = ActiveValue::set(Some(chrono::Local::now().into()));
        }
        Ok(active.update(db).await?)
    }
}
