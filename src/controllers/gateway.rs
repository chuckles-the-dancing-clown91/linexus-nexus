#![allow(clippy::missing_errors_doc)]
//! # Daedalus IT gateway (`api/v1`)
//!
//! The single surface Daedalus IT talks to. It mirrors the contract in
//! `daedalus-it/apps/api/internal/linexus/client.go` exactly — agent inventory,
//! machine facts, log tails (fetched from the Logger), and task dispatch
//! (planned by the Orchestrator). The same surface carries the agent's own
//! enroll / report / heartbeat calls.
//!
//! Auth is a bearer token (`Authorization: Bearer <key>`) validated against the
//! Nexus system-token set, so Daedalus IT and agents present the Nexus API key
//! the same way. Response field names are camelCase to match the Go client's
//! JSON tags; where a stored value is absent it is rendered as an empty string
//! or zero rather than `null`, so the Go structs always decode.

use axum::extract::{Path, Query};
use axum::http::HeaderMap;
use chrono::{Duration, Utc};
use loco_rs::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::gateway_client;
use crate::middleware::system_token;
use crate::models::{agents, tasks};

// ---------------------------------------------------------------------------
// Projection to the Daedalus IT contract
// ---------------------------------------------------------------------------

fn parse_uuid(raw: &str) -> Result<uuid::Uuid> {
    uuid::Uuid::parse_str(raw)
        .map_err(|e| loco_rs::Error::BadRequest(format!("invalid agent id: {e}")))
}

/// The timestamp Daedalus IT shows as `lastReportAt`. Always a valid RFC 3339
/// string (the Go `time.Time` field can't decode an empty value): falls back
/// through heartbeat → enrollment → row creation.
fn last_report_at(a: &agents::Model) -> String {
    a.last_heartbeat_at
        .or(a.enrolled_at)
        .unwrap_or(a.created_at)
        .to_rfc3339()
}

/// Derive the `healthy | drift | offline` state Daedalus IT expects. An agent
/// that reported within the last 10 minutes is healthy; a stale or never-seen
/// agent is offline; an explicitly drifted agent is surfaced as such.
fn agent_state(a: &agents::Model) -> String {
    if a.status == "drift" {
        return "drift".to_string();
    }
    match a.last_heartbeat_at {
        Some(t) => {
            let age = Utc::now().signed_duration_since(t.with_timezone(&Utc));
            if age <= Duration::minutes(10) {
                "healthy".to_string()
            } else {
                "offline".to_string()
            }
        }
        None => "offline".to_string(),
    }
}

/// The `Agent` shape: identity + live state.
fn agent_json(a: &agents::Model) -> Value {
    json!({
        "id": a.agent_id.to_string(),
        "hostname": a.hostname,
        "hostgroup": a.hostgroup.clone().unwrap_or_default(),
        "state": agent_state(a),
        "lastReportAt": last_report_at(a),
    })
}

/// The `AgentDetail` shape: identity + state + reported facts.
fn agent_detail_json(a: &agents::Model) -> Value {
    json!({
        "id": a.agent_id.to_string(),
        "hostname": a.hostname,
        "hostgroup": a.hostgroup.clone().unwrap_or_default(),
        "state": agent_state(a),
        "lastReportAt": last_report_at(a),
        "os": a.os.clone().unwrap_or_default(),
        "kernel": a.kernel.clone().unwrap_or_default(),
        "arch": a.arch.clone().unwrap_or_default(),
        "cpuCores": a.cpu_cores.unwrap_or(0),
        "memoryMb": a.memory_mb.unwrap_or(0),
        "diskGb": a.disk_gb.unwrap_or(0),
        "agentVersion": a.agent_version.clone().unwrap_or_default(),
        "uptimeSeconds": a.uptime_seconds.unwrap_or(0),
    })
}

// ---------------------------------------------------------------------------
// Daedalus IT read/dispatch surface
// ---------------------------------------------------------------------------

/// `GET /api/v1/agents` — agent inventory.
pub async fn list_agents(State(ctx): State<AppContext>, headers: HeaderMap) -> Result<Response> {
    system_token::authenticate_bearer(&ctx, &headers).await?;
    let all = agents::Model::find_all(&ctx.db).await?;
    let out: Vec<Value> = all.iter().map(agent_json).collect();
    format::json(out)
}

/// `GET /api/v1/agents/{id}` — one agent's full record.
pub async fn get_agent(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response> {
    system_token::authenticate_bearer(&ctx, &headers).await?;
    let uuid = parse_uuid(&id)?;
    let agent = agents::Model::find_by_agent_id(&ctx.db, &uuid).await?;
    format::json(agent_detail_json(&agent))
}

#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    pub limit: Option<i64>,
}

/// `GET /api/v1/agents/{id}/logs?limit=N` — tail the agent's journal, fetched
/// from the Logger and projected to Daedalus IT's `LogLine`.
pub async fn agent_logs(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<LogsQuery>,
) -> Result<Response> {
    system_token::authenticate_bearer(&ctx, &headers).await?;
    let uuid = parse_uuid(&id)?;
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);

    let records = gateway_client::fetch_agent_logs(&uuid.to_string(), limit)
        .await
        .map_err(|e| loco_rs::Error::Any(e.into()))?;

    let lines: Vec<Value> = records
        .iter()
        .map(|r| {
            json!({
                "timestamp": r.get("timestamp").and_then(Value::as_str).unwrap_or_default(),
                "level": r.get("level").and_then(Value::as_str).unwrap_or("info"),
                "source": r.get("source").and_then(Value::as_str).unwrap_or("agent"),
                "message": r.get("message").and_then(Value::as_str).unwrap_or_default(),
            })
        })
        .collect();
    format::json(lines)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTaskRequest {
    pub intent: String,
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(default)]
    pub requester_id: String,
    #[serde(default)]
    pub auto_rollback: bool,
    #[serde(default)]
    pub params: std::collections::BTreeMap<String, String>,
}

/// `POST /api/v1/tasks` — record a task, have the Orchestrator plan it, and
/// return `{taskId, status}`. If the Orchestrator is unreachable the task is
/// still recorded (status `accepted`) so nothing is silently lost.
pub async fn create_task(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Json(req): Json<CreateTaskRequest>,
) -> Result<Response> {
    let svc = system_token::authenticate_bearer(&ctx, &headers).await?;
    let created_by = if req.requester_id.is_empty() {
        format!("service:{}", svc.service)
    } else {
        req.requester_id.clone()
    };

    let params = tasks::CreateTaskParams {
        intent: req.intent.clone(),
        target_agents: Some(req.targets.clone()),
    };
    let task = tasks::Model::create(&ctx.db, &created_by, &params).await?;

    let plan_body = json!({
        "intent": req.intent,
        "targets": req.targets,
        "requester_id": created_by,
        "auto_rollback": req.auto_rollback,
        "params": req.params,
        "task_id": task.task_id.to_string(),
    });

    let task = match gateway_client::plan_task(&plan_body).await {
        Ok(plan) => {
            let plan_str = serde_json::to_string(&plan).unwrap_or_default();
            task.set_plan(&ctx.db, &plan_str, "planned").await?
        }
        Err(e) => {
            tracing::warn!(error = %e, task_id = %task.task_id, "orchestrator planning failed; task recorded unplanned");
            task.update_status(&ctx.db, "accepted").await?
        }
    };

    // Best-effort: surface the task in the operational log so it appears when
    // Daedalus IT tails the journal. A logging failure never fails the request.
    let audit = json!({
        "task_id": task.task_id.to_string(),
        "level": "info",
        "source": "nexus",
        "message": format!("task planned: {}", req.intent),
        "metadata": {
            "intent": req.intent,
            "targets": req.targets,
            "requester": created_by,
            "status": task.status,
        },
    });
    if let Err(e) = gateway_client::ship_log(&audit).await {
        tracing::warn!(error = %e, "failed to ship task audit log");
    }

    format::json(json!({ "taskId": task.task_id.to_string(), "status": task.status }))
}

// ---------------------------------------------------------------------------
// Agent-facing surface (enroll / report / heartbeat)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollRequest {
    pub hostname: String,
    pub hostgroup: Option<String>,
    pub capability_manifest: Option<String>,
}

/// `POST /api/v1/agents/enroll` — register a machine, returning its full record
/// (the agent persists the assigned `id` and uses it for report/heartbeat).
pub async fn enroll(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Json(req): Json<EnrollRequest>,
) -> Result<Response> {
    system_token::authenticate_bearer(&ctx, &headers).await?;
    let params = agents::EnrollAgentParams {
        hostname: req.hostname,
        hostgroup: req.hostgroup,
        capability_manifest: req.capability_manifest,
    };
    let agent = agents::Model::enroll(&ctx.db, &params).await?;
    tracing::info!(agent_id = %agent.agent_id, hostname = %agent.hostname, "agent enrolled");
    format::json(agent_detail_json(&agent))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportRequest {
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

/// `POST /api/v1/agents/{id}/report` — apply a facts report (also a heartbeat).
pub async fn report(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<ReportRequest>,
) -> Result<Response> {
    system_token::authenticate_bearer(&ctx, &headers).await?;
    let uuid = parse_uuid(&id)?;
    let agent = agents::Model::find_by_agent_id(&ctx.db, &uuid).await?;
    let params = agents::ReportFactsParams {
        hostname: req.hostname,
        hostgroup: req.hostgroup,
        os: req.os,
        kernel: req.kernel,
        arch: req.arch,
        cpu_cores: req.cpu_cores,
        memory_mb: req.memory_mb,
        disk_gb: req.disk_gb,
        agent_version: req.agent_version,
        uptime_seconds: req.uptime_seconds,
        capability_manifest: req.capability_manifest,
    };
    let updated = agent.report_facts(&ctx.db, &params).await?;
    format::json(agent_detail_json(&updated))
}

/// `POST /api/v1/agents/{id}/heartbeat` — liveness signal.
pub async fn heartbeat(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response> {
    system_token::authenticate_bearer(&ctx, &headers).await?;
    let uuid = parse_uuid(&id)?;
    let agent = agents::Model::find_by_agent_id(&ctx.db, &uuid).await?;
    let updated = agent.heartbeat(&ctx.db).await?;
    format::json(agent_json(&updated))
}

pub fn routes() -> Routes {
    Routes::new()
        .prefix("api/v1")
        .add("/agents", get(list_agents))
        .add("/agents/enroll", post(enroll))
        .add("/agents/{id}", get(get_agent))
        .add("/agents/{id}/report", post(report))
        .add("/agents/{id}/heartbeat", post(heartbeat))
        .add("/agents/{id}/logs", get(agent_logs))
        .add("/tasks", post(create_task))
}
