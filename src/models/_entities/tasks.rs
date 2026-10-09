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
    /// The orchestrator's TransactionPlan (JSON), stored when the task is planned.
    #[sea_orm(column_type = "Text", nullable)]
    pub plan: Option<String>,
    /// The agent's own verdict (`success` / `failed`), set when it reports.
    pub result_status: Option<String>,
    /// The agent's one-line summary of the run.
    #[sea_orm(column_type = "Text", nullable)]
    pub result_message: Option<String>,
    /// Overall exit code reported (or derived) for the run.
    pub exit_code: Option<i64>,
    /// Combined output, capped at [`crate::models::tasks::OUTPUT_CAP`] bytes.
    #[sea_orm(column_type = "Text", nullable)]
    pub output: Option<String>,
    /// Per-step results (JSON array of `{id, action, status, changed, output, error}`).
    #[sea_orm(column_type = "Text", nullable)]
    pub steps: Option<String>,
    /// The body sent to the Orchestrator's `POST /plan`, kept so an
    /// `accepted` (unplanned) task can be re-planned exactly as asked.
    #[sea_orm(column_type = "Text", nullable)]
    pub plan_request: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
