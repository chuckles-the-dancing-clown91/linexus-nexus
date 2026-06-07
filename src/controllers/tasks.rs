#![allow(clippy::missing_errors_doc)]
#![allow(clippy::unnecessary_struct_initialization)]
#![allow(clippy::unused_async)]
use axum::extract::Path;
use loco_rs::prelude::*;
use serde::{Deserialize, Serialize};

use crate::middleware::{permission_funnel, rbac};
use crate::models::tasks;

#[derive(Debug, Deserialize)]
pub struct CreateTaskRequest {
    pub intent: String,
    pub target_agents: Option<Vec<String>>,
    pub step_up_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TaskResponse {
    pub task_id: String,
    pub intent: String,
    pub status: String,
    pub created_by: String,
    pub created_at: String,
}

impl From<tasks::Model> for TaskResponse {
    fn from(task: tasks::Model) -> Self {
        Self {
            task_id: task.task_id.to_string(),
            intent: task.intent,
            status: task.status,
            created_by: task.created_by,
            created_at: task.created_at.to_string(),
        }
    }
}

/// List all tasks
pub async fn list(auth: auth::JWT, State(ctx): State<AppContext>) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "tasks:read").await?;

    let all_tasks = tasks::Model::find_all(&ctx.db).await?;
    let response: Vec<TaskResponse> = all_tasks.into_iter().map(TaskResponse::from).collect();
    format::json(response)
}

/// Create a new task
pub async fn create(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
    Json(req): Json<CreateTaskRequest>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;

    // Evaluate Permission Funnel
    let intent = permission_funnel::OperationIntent::new("tasks:create");
    let funnel_result =
        permission_funnel::evaluate(&ctx.db, user.id, &intent, req.step_up_token.as_deref())
            .await?;

    if !funnel_result.allowed {
        return Err(loco_rs::Error::Unauthorized(
            funnel_result
                .reason
                .unwrap_or_else(|| "Permission denied".to_string()),
        ));
    }

    let params = tasks::CreateTaskParams {
        intent: req.intent,
        target_agents: req.target_agents,
    };

    let task = tasks::Model::create(&ctx.db, &user.pid.to_string(), &params).await?;
    format::json(TaskResponse::from(task))
}

/// Get a single task
pub async fn get_one(
    auth: auth::JWT,
    Path(task_id): Path<String>,
    State(ctx): State<AppContext>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "tasks:read").await?;

    let uuid = uuid::Uuid::parse_str(&task_id).map_err(|e| loco_rs::Error::Any(e.into()))?;
    let task = tasks::Model::find_by_task_id(&ctx.db, &uuid).await?;
    format::json(TaskResponse::from(task))
}

/// Cancel a task
pub async fn cancel(
    auth: auth::JWT,
    Path(task_id): Path<String>,
    State(ctx): State<AppContext>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "tasks:cancel").await?;

    let uuid = uuid::Uuid::parse_str(&task_id).map_err(|e| loco_rs::Error::Any(e.into()))?;
    let task = tasks::Model::find_by_task_id(&ctx.db, &uuid).await?;
    let updated = task.update_status(&ctx.db, "cancelled").await?;
    format::json(TaskResponse::from(updated))
}

pub fn routes() -> Routes {
    Routes::new()
        .prefix("api/tasks")
        .add("/", get(list))
        .add("/", post(create))
        .add("/{task_id}", get(get_one))
        .add("/{task_id}/cancel", post(cancel))
}
