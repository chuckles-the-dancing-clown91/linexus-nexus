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

    /// Store the orchestrator's plan (JSON) and set the status in one update.
    pub async fn set_plan(
        self,
        db: &DatabaseConnection,
        plan_json: &str,
        status: &str,
    ) -> ModelResult<Self> {
        let mut active: tasks::ActiveModel = self.into();
        active.plan = ActiveValue::set(Some(plan_json.to_string()));
        active.status = ActiveValue::set(status.to_string());
        Ok(active.update(db).await?)
    }

    /// Tasks targeting `agent_id` that are planned or already dispatched (i.e.
    /// ready for the agent to pick up and not yet completed). `target_agents` is
    /// a JSON array; membership is checked in-process since the set is small.
    pub async fn find_pending_for_agent(
        db: &DatabaseConnection,
        agent_id: &str,
    ) -> ModelResult<Vec<Self>> {
        let candidates = tasks::Entity::find()
            .filter(tasks::Column::Status.is_in(["planned", "dispatched"]))
            .all(db)
            .await?;
        Ok(candidates
            .into_iter()
            .filter(|t| task_targets_agent(t, agent_id))
            .collect())
    }

    /// Transition a freshly planned task to `dispatched` once an agent has been
    /// handed the plan. Idempotent: only `planned` tasks move.
    pub async fn mark_dispatched(self, db: &DatabaseConnection) -> ModelResult<Self> {
        if self.status != "planned" {
            return Ok(self);
        }
        let mut active: tasks::ActiveModel = self.into();
        active.status = ActiveValue::set("dispatched".to_string());
        Ok(active.update(db).await?)
    }

    /// Record a terminal result reported by the agent: set `completed`/`failed`,
    /// stamp completion, and store any error message.
    pub async fn complete(
        self,
        db: &DatabaseConnection,
        status: &str,
        error: Option<&str>,
    ) -> ModelResult<Self> {
        let mut active: tasks::ActiveModel = self.into();
        active.status = ActiveValue::set(status.to_string());
        active.completed_at = ActiveValue::set(Some(chrono::Local::now().into()));
        if let Some(e) = error {
            active.error_message = ActiveValue::set(Some(e.to_string()));
        }
        Ok(active.update(db).await?)
    }
}

/// Whether a task's `target_agents` JSON array names `agent_id`.
fn task_targets_agent(t: &Model, agent_id: &str) -> bool {
    match &t.target_agents {
        Some(s) => serde_json::from_str::<Vec<String>>(s)
            .map(|v| v.iter().any(|a| a == agent_id))
            .unwrap_or(false),
        None => false,
    }
}
