#![allow(clippy::missing_errors_doc)]
#![allow(clippy::unnecessary_struct_initialization)]
#![allow(clippy::unused_async)]
use axum::extract::Path;
use loco_rs::prelude::*;
use serde::{Deserialize, Serialize};

use crate::middleware::rbac;
use crate::models::agents;

#[derive(Debug, Deserialize)]
pub struct EnrollRequest {
    pub hostname: String,
    pub capability_manifest: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AgentResponse {
    pub agent_id: String,
    pub hostname: String,
    pub status: String,
    pub enrolled_at: Option<String>,
    pub last_heartbeat_at: Option<String>,
}

impl From<agents::Model> for AgentResponse {
    fn from(agent: agents::Model) -> Self {
        Self {
            agent_id: agent.agent_id.to_string(),
            hostname: agent.hostname,
            status: agent.status,
            enrolled_at: agent.enrolled_at.map(|d| d.to_string()),
            last_heartbeat_at: agent.last_heartbeat_at.map(|d| d.to_string()),
        }
    }
}

/// List all agents
pub async fn list(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "agents:read").await?;

    let all_agents = agents::Model::find_all(&ctx.db).await?;
    let response: Vec<AgentResponse> = all_agents.into_iter().map(AgentResponse::from).collect();
    format::json(response)
}

/// Get a single agent
pub async fn get_one(
    auth: auth::JWT,
    Path(agent_id): Path<String>,
    State(ctx): State<AppContext>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "agents:read").await?;

    let uuid = uuid::Uuid::parse_str(&agent_id)
        .map_err(|e| loco_rs::Error::Any(e.into()))?;
    let agent = agents::Model::find_by_agent_id(&ctx.db, &uuid).await?;
    format::json(AgentResponse::from(agent))
}

/// Enroll a new agent
pub async fn enroll(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
    Json(req): Json<EnrollRequest>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "agents:enroll").await?;

    let params = agents::EnrollAgentParams {
        hostname: req.hostname,
        capability_manifest: req.capability_manifest,
    };

    let agent = agents::Model::enroll(&ctx.db, &params).await?;
    format::json(AgentResponse::from(agent))
}

/// Record agent heartbeat
pub async fn heartbeat(
    Path(agent_id): Path<String>,
    State(ctx): State<AppContext>,
) -> Result<Response> {
    let uuid = uuid::Uuid::parse_str(&agent_id)
        .map_err(|e| loco_rs::Error::Any(e.into()))?;
    let agent = agents::Model::find_by_agent_id(&ctx.db, &uuid).await?;
    let updated = agent.heartbeat(&ctx.db).await?;
    format::json(AgentResponse::from(updated))
}

pub fn routes() -> Routes {
    Routes::new()
        .prefix("api/agents")
        .add("/", get(list))
        .add("/", post(enroll))
        .add("/{agent_id}", get(get_one))
        .add("/{agent_id}/heartbeat", post(heartbeat))
}
