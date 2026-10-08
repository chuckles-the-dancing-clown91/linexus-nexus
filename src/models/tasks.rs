use loco_rs::prelude::*;
use sea_orm::prelude::DateTimeWithTimeZone;
use sea_orm::ActiveValue;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use super::_entities::tasks::{self, ActiveModel, Entity, Model};

/// Most bytes of combined output kept per task. Longer output keeps its tail
/// (where a failing script says why) behind a truncation marker.
pub const OUTPUT_CAP: usize = 64 * 1024;
/// Most bytes of output kept per step.
pub const STEP_OUTPUT_CAP: usize = 16 * 1024;
/// Most bytes of a step's error kept.
pub const STEP_ERROR_CAP: usize = 4 * 1024;
/// Most steps kept per result; a plan is a handful of steps, so this only
/// bounds a misbehaving reporter.
pub const MAX_STEPS: usize = 256;

/// Keep at most `max` bytes of `s`. When it is longer, the *tail* is kept and
/// prefixed with a marker saying how much was dropped; the cut always lands on
/// a UTF-8 character boundary, so the result may be a few bytes short of `max`.
#[must_use]
pub fn cap_output(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let marker_room = 48;
    let keep = max.saturating_sub(marker_room);
    let mut start = s.len() - keep;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("[... truncated {start} bytes ...]\n{}", &s[start..])
}

/// One step's outcome as stored and served: `status` is the agent's verdict
/// for the step (the rmm-agent sends `success`, `failed` or `skipped`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepResult {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub changed: bool,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub error: String,
}

impl StepResult {
    /// Apply the per-step size caps.
    #[must_use]
    pub fn capped(mut self) -> Self {
        self.output = cap_output(&self.output, STEP_OUTPUT_CAP);
        self.error = cap_output(&self.error, STEP_ERROR_CAP);
        self
    }
}

/// A terminal result to record on a task.
#[derive(Debug, Clone, Default)]
pub struct RecordResultParams {
    /// Lifecycle status to move to: `completed` or `failed`.
    pub status: String,
    /// The agent's verdict as sent (`success` / `failed`).
    pub result_status: String,
    pub error: Option<String>,
    pub message: Option<String>,
    pub exit_code: i64,
    /// Combined output; capped to [`OUTPUT_CAP`] on write.
    pub output: Option<String>,
    /// Per-step results; capped to [`MAX_STEPS`] and the per-step limits.
    pub steps: Vec<StepResult>,
}

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
        active.updated_at = ActiveValue::set(chrono::Local::now().into());
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
        active.updated_at = ActiveValue::set(chrono::Local::now().into());
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
        active.updated_at = ActiveValue::set(chrono::Local::now().into());
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
        let now: DateTimeWithTimeZone = chrono::Local::now().into();
        active.status = ActiveValue::set(status.to_string());
        active.completed_at = ActiveValue::set(Some(now));
        active.updated_at = ActiveValue::set(now);
        if let Some(e) = error {
            active.error_message = ActiveValue::set(Some(e.to_string()));
        }
        Ok(active.update(db).await?)
    }

    /// Record the agent's full result: the lifecycle status (as [`complete`]
    /// does) plus its verdict, exit code, capped output and per-step results,
    /// so the task can be read back with `GET /api/v1/tasks/{id}`. A second
    /// report for the same task overwrites the first.
    ///
    /// [`complete`]: Self::complete
    pub async fn record_result(
        self,
        db: &DatabaseConnection,
        params: &RecordResultParams,
    ) -> ModelResult<Self> {
        let steps: Vec<StepResult> = params
            .steps
            .iter()
            .take(MAX_STEPS)
            .cloned()
            .map(StepResult::capped)
            .collect();
        let steps_json = if params.steps.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&steps).unwrap_or_else(|_| "[]".to_string()))
        };

        let mut active: tasks::ActiveModel = self.into();
        let now: DateTimeWithTimeZone = chrono::Local::now().into();
        active.status = ActiveValue::set(params.status.clone());
        active.completed_at = ActiveValue::set(Some(now));
        active.updated_at = ActiveValue::set(now);
        if let Some(e) = &params.error {
            active.error_message = ActiveValue::set(Some(e.clone()));
        }
        active.result_status = ActiveValue::set(Some(params.result_status.clone()));
        active.result_message = ActiveValue::set(params.message.clone());
        active.exit_code = ActiveValue::set(Some(params.exit_code));
        active.output =
            ActiveValue::set(params.output.as_deref().map(|o| cap_output(o, OUTPUT_CAP)));
        active.steps = ActiveValue::set(steps_json);
        Ok(active.update(db).await?)
    }

    /// The task's targets (agent ids), decoded from the stored JSON array.
    #[must_use]
    pub fn targets(&self) -> Vec<String> {
        self.target_agents
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .unwrap_or_default()
    }

    /// The stored per-step results, decoded (empty when none were reported).
    #[must_use]
    pub fn step_results(&self) -> Vec<StepResult> {
        self.steps
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<StepResult>>(s).ok())
            .unwrap_or_default()
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
