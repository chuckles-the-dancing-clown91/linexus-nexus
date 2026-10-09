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

use crate::controllers::api::{ApiError, ApiJson, ApiResult};
use crate::dispatch::{self, DispatchRequest};
use crate::gateway_client;
use crate::middleware::system_token::{self, Caller};
use crate::models::{agents, enrollment_tokens, tasks};

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
pub fn agent_state(a: &agents::Model) -> String {
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

/// The environment/tracking shape, shared by the read endpoint and the detail
/// projection. `environment` is never empty on the wire — an agent enrolled
/// before this column existed reads back as production, which is the same
/// assumption every other part of the stack makes about an unclassified box.
fn environment_json(a: &agents::Model) -> Value {
    let env = if a.environment.trim().is_empty() {
        agents::DEFAULT_ENVIRONMENT.to_string()
    } else {
        a.environment.clone()
    };
    json!({
        "environment": env,
        "monitored": a.monitored,
        "note": a.monitor_note.clone().unwrap_or_default(),
        "updatedAt": a.environment_updated_at.map(|d| d.to_rfc3339()).unwrap_or_default(),
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
        // Policy, carried alongside the facts so the Hub can see at a glance
        // whether the two ends actually agree about what this machine is.
        "environment": if a.environment.trim().is_empty() {
            agents::DEFAULT_ENVIRONMENT.to_string()
        } else {
            a.environment.clone()
        },
        "monitored": a.monitored,
        "monitorNote": a.monitor_note.clone().unwrap_or_default(),
        // Richer facts (§2). Absent lists are `[]`, an absent DNS server and
        // a never-reported `factsAt` are `null`.
        "machineId": a.machine_id.clone().unwrap_or_default(),
        "publicIp": a.public_ip.clone().unwrap_or_default(),
        "interfaces": agents::json_column(a.interfaces.as_deref(), json!([])),
        "listening": agents::json_column(a.listening.as_deref(), json!([])),
        "dnsServer": agents::json_column(a.dns_server.as_deref(), Value::Null),
        "factsAt": a.facts_at.map_or(Value::Null, |d| json!(d.to_rfc3339())),
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

/// `GET /api/v1/agents/{id}/services` — the services in the last report.
pub async fn agent_services(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    system_token::authenticate_bearer(&ctx, &headers).await?;
    let agent = find_agent(&ctx, &id).await?;
    Ok(axum::Json(json!({
        "services": agents::json_column(agent.services.as_deref(), json!([])),
    }))
    .into_response())
}

/// `GET /api/v1/agents/{id}/packages` — the packages in the last report.
pub async fn agent_packages(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    system_token::authenticate_bearer(&ctx, &headers).await?;
    let agent = find_agent(&ctx, &id).await?;
    Ok(axum::Json(json!({
        "packages": agents::json_column(agent.packages.as_deref(), json!([])),
    }))
    .into_response())
}

/// The agent named by a path id, or `404 not_found`.
async fn find_agent(ctx: &AppContext, id: &str) -> ApiResult<agents::Model> {
    let uuid = uuid::Uuid::parse_str(id).map_err(|_| ApiError::not_found("no such agent"))?;
    agents::Model::find_by_agent_id(&ctx.db, &uuid)
        .await
        .map_err(|e| match e {
            ModelError::EntityNotFound => ApiError::not_found("no such agent"),
            other => other.into(),
        })
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
/// still recorded (status `accepted`) and re-planned later (see
/// [`crate::dispatch`]). A `hostgroup:<name>` target expands to every agent of
/// that hostgroup now.
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
    let task = dispatch::dispatch(
        &ctx,
        &DispatchRequest {
            intent: req.intent,
            targets: req.targets,
            requester: created_by,
            auto_rollback: req.auto_rollback,
            params: req.params,
        },
    )
    .await?;
    format::json(json!({ "taskId": task.task_id.to_string(), "status": task.status }))
}

/// `POST /api/v1/tasks/{id}/cancel` — cancel a task that has not finished.
/// A cancelled task is never handed to an agent. `409` when it is already
/// terminal.
pub async fn cancel_task(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    system_token::authenticate_bearer(&ctx, &headers).await?;
    let tid = uuid::Uuid::parse_str(&id).map_err(|_| ApiError::not_found("no such task"))?;
    match dispatch::cancel(&ctx, &tid).await {
        Ok(true) => Ok(
            axum::Json(json!({ "taskId": tid.to_string(), "status": "cancelled" })).into_response(),
        ),
        Ok(false) => Err(ApiError::conflict("the task has already finished")),
        Err(ModelError::EntityNotFound) => Err(ApiError::not_found("no such task")),
        Err(e) => Err(e.into()),
    }
}

// ---------------------------------------------------------------------------
// Agent-facing surface (enroll / report / heartbeat)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollRequest {
    pub hostname: String,
    #[serde(default)]
    pub hostgroup: Option<String>,
    #[serde(default)]
    pub capability_manifest: Option<String>,
    /// `/etc/machine-id`: re-adopts an existing agent with the same one.
    #[serde(default)]
    pub machine_id: Option<String>,
    /// An `nxe_` enrollment token; authenticates the request without a bearer.
    #[serde(default)]
    pub enrollment_token: Option<String>,
}

fn clean_field(v: Option<&str>, field: &str, max: usize) -> ApiResult<Option<String>> {
    match v.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) if s.len() > max || s.chars().any(char::is_control) => Err(ApiError::invalid(
            format!("{field}: at most {max} printable characters"),
        )),
        Some(s) => Ok(Some(s.to_string())),
    }
}

/// `POST /api/v1/agents/enroll` — register a machine and give it its own
/// credential.
///
/// Authenticated **either** by a system key in the bearer (the legacy path)
/// **or** by an `enrollmentToken` in the body (no bearer needed). A token's
/// hostgroup and environment win over the body. A `machineId` that is
/// already known re-adopts that agent (same agent id, credential rotated) —
/// except, on the token path, when that agent belongs to another hostgroup
/// (`409`), so a client's token cannot take over another client's machine.
/// Answers `201` with the agent record plus `agentToken` (`nxa_…`, shown
/// once), which the agent presents on every later call.
pub async fn enroll(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    ApiJson(req): ApiJson<EnrollRequest>,
) -> ApiResult<Response> {
    let hostname = clean_field(Some(&req.hostname), "hostname", 253)?
        .ok_or_else(|| ApiError::invalid("hostname: required"))?;
    let hostgroup = clean_field(req.hostgroup.as_deref(), "hostgroup", 128)?;
    let machine_id = clean_field(req.machine_id.as_deref(), "machineId", 128)?;
    if machine_id
        .as_deref()
        .is_some_and(|m| !m.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
    {
        return Err(ApiError::invalid(
            "machineId: letters, digits and dashes only",
        ));
    }
    let token = clean_field(req.enrollment_token.as_deref(), "enrollmentToken", 128)?;
    let existing = match &machine_id {
        Some(m) => agents::Model::find_by_machine_id(&ctx.db, m).await?,
        None => None,
    };

    let mut params = agents::EnrollAgentParams {
        hostname,
        hostgroup,
        capability_manifest: req.capability_manifest,
        machine_id,
        ..Default::default()
    };
    let token_row = if let Some(plaintext) = token {
        let refused = |r: enrollment_tokens::Refusal| {
            ApiError::unauthorized(match r {
                enrollment_tokens::Refusal::Unknown => "unknown enrollment token",
                enrollment_tokens::Refusal::Expired => "enrollment token expired",
                enrollment_tokens::Refusal::Revoked => "enrollment token revoked",
                enrollment_tokens::Refusal::UsedUp => "enrollment token used up",
            })
        };
        let peek = enrollment_tokens::Model::peek(&ctx.db, &plaintext)
            .await?
            .map_err(refused)?;
        if let Some(a) = &existing {
            let theirs = a.hostgroup.as_deref().unwrap_or_default();
            if !theirs.is_empty() && theirs != peek.hostgroup {
                return Err(ApiError::conflict(
                    "this machine is enrolled in another hostgroup; re-enroll it with a system key or retire the old agent",
                ));
            }
        }
        let row = enrollment_tokens::Model::consume(&ctx.db, &plaintext)
            .await?
            .map_err(refused)?;
        params.hostgroup = Some(row.hostgroup.clone());
        params.environment = Some(row.environment.clone());
        params.enrollment_token_id = Some(row.token_id);
        params.metadata = Some(row.metadata_value());
        Some(row)
    } else {
        system_token::authenticate_bearer(&ctx, &headers).await?;
        None
    };

    let readopted = existing.is_some();
    let agent = match existing {
        Some(a) => a.readopt(&ctx.db, &params).await?,
        None => agents::Model::enroll(&ctx.db, &params).await?,
    };
    let (agent, agent_token) = agent.rotate_credential(&ctx.db).await?;
    if let Some(row) = token_row.clone() {
        row.add_agent(&ctx.db, &agent.agent_id).await?;
    }
    tracing::info!(
        agent_id = %agent.agent_id,
        hostname = %agent.hostname,
        readopted,
        via_token = token_row.is_some(),
        "agent enrolled"
    );

    let mut body = agent_detail_json(&agent);
    if let Value::Object(map) = &mut body {
        map.insert("agentId".into(), json!(agent.agent_id.to_string()));
        map.insert("agentToken".into(), json!(agent_token));
        map.insert(
            "enrollmentTokenId".into(),
            json!(token_row
                .as_ref()
                .map(|t| t.token_id.to_string())
                .unwrap_or_default()),
        );
        map.insert(
            "metadata".into(),
            agents::json_column(agent.metadata.as_deref(), json!({})),
        );
        map.insert("readopted".into(), json!(readopted));
    }
    Ok((axum::http::StatusCode::CREATED, axum::Json(body)).into_response())
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
    // Richer facts (§2), all optional.
    #[serde(default)]
    pub machine_id: Option<String>,
    #[serde(default)]
    pub public_ip: Option<String>,
    #[serde(default)]
    pub interfaces: Option<Value>,
    #[serde(default)]
    pub listening: Option<Value>,
    #[serde(default)]
    pub services: Option<Value>,
    #[serde(default)]
    pub packages: Option<Value>,
    #[serde(default)]
    pub dns_server: Option<Value>,
}

/// Keep a reported list only when it is a JSON array (anything else is
/// ignored rather than stored and served back as garbage).
fn array_only(v: Option<Value>) -> Option<Value> {
    v.filter(Value::is_array)
}

/// `POST /api/v1/agents/{id}/report` — apply a facts report (also a heartbeat).
pub async fn report(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<ReportRequest>,
) -> Result<Response> {
    let uuid = parse_uuid(&id)?;
    system_token::authorize_agent(&ctx, &headers, &uuid).await?;
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
        machine_id: req
            .machine_id
            .filter(|m| m.len() <= 128 && m.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')),
        public_ip: req
            .public_ip
            .filter(|ip| ip.trim().parse::<std::net::IpAddr>().is_ok())
            .map(|ip| ip.trim().to_string()),
        interfaces: array_only(req.interfaces),
        listening: array_only(req.listening),
        services: array_only(req.services),
        packages: array_only(req.packages),
        dns_server: req.dns_server.filter(Value::is_object),
    };
    let updated = agent.report_facts(&ctx.db, &params).await?;
    format::json(agent_detail_json(&updated))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetEnvironmentRequest {
    pub environment: String,
    #[serde(default = "default_monitored")]
    pub monitored: bool,
    #[serde(default)]
    pub note: Option<String>,
}

/// Absent means tracked. A body that forgets the field must not silently mute
/// a machine — going quiet has to be something somebody asked for.
const fn default_monitored() -> bool {
    true
}

/// `POST /api/v1/agents/{id}/environment` — record the Hub's decision about
/// what this machine is and whether it counts.
///
/// This is the durable half of the push. The matching `set_environment` intent
/// tells the *running* agent to apply it now; this makes sure the answer
/// survives the agent being offline, restarted, or reinstalled, because the
/// enrolment record is what a fresh agent reads itself out of.
pub async fn set_environment(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<SetEnvironmentRequest>,
) -> Result<Response> {
    system_token::authenticate_bearer(&ctx, &headers).await?;
    let uuid = parse_uuid(&id)?;
    let agent = agents::Model::find_by_agent_id(&ctx.db, &uuid).await?;

    let params = agents::SetEnvironmentParams {
        environment: req.environment,
        monitored: req.monitored,
        note: req.note,
    };
    let updated = agent.set_environment(&ctx.db, &params).await?;
    tracing::info!(
        agent_id = %updated.agent_id,
        hostname = %updated.hostname,
        environment = %updated.environment,
        monitored = updated.monitored,
        "agent environment set"
    );

    // Best-effort audit line, so the decision appears in the journal Daedalus
    // IT tails rather than only in this table. A logging failure never fails
    // the write — the inventory is already correct.
    let audit = json!({
        "agent_id": updated.agent_id.to_string(),
        "level": "info",
        "source": "nexus",
        "message": format!(
            "environment set to {} ({})",
            updated.environment,
            if updated.monitored { "tracked" } else { "not tracked" }
        ),
        "metadata": {
            "environment": updated.environment,
            "monitored": updated.monitored,
            "note": updated.monitor_note,
        },
    });
    if let Err(e) = gateway_client::ship_log(&audit).await {
        tracing::warn!(error = %e, "failed to ship environment audit log");
    }

    format::json(environment_json(&updated))
}

/// `GET /api/v1/agents/{id}/environment` — what the inventory authority
/// currently believes this machine is. Read by the Hub to confirm the two ends
/// agree, and by an agent that wants to re-read its own state after a restart.
pub async fn get_environment(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response> {
    let uuid = parse_uuid(&id)?;
    system_token::authorize_agent(&ctx, &headers, &uuid).await?;
    let agent = agents::Model::find_by_agent_id(&ctx.db, &uuid).await?;
    format::json(environment_json(&agent))
}

/// `POST /api/v1/agents/{id}/heartbeat` — liveness signal.
pub async fn heartbeat(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response> {
    let uuid = parse_uuid(&id)?;
    system_token::authorize_agent(&ctx, &headers, &uuid).await?;
    let agent = agents::Model::find_by_agent_id(&ctx.db, &uuid).await?;
    let updated = agent.heartbeat(&ctx.db).await?;
    format::json(agent_json(&updated))
}

// ---------------------------------------------------------------------------
// Agent task loop: poll for work, report results, ship logs (all via Nexus, so
// the agent never talks to the Orchestrator or Logger directly).
// ---------------------------------------------------------------------------

/// `GET /api/v1/agents/{id}/tasks` — planned/dispatched tasks targeting this
/// agent, each with its TransactionPlan. Handing a plan over transitions the
/// task from `planned` to `dispatched`. Tasks for this agent that are still
/// `accepted` (the Orchestrator was unreachable) are re-planned first, so the
/// agent gets them on this poll once the Orchestrator is back.
pub async fn poll_tasks(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response> {
    let uuid = parse_uuid(&id)?;
    system_token::authorize_agent(&ctx, &headers, &uuid).await?;
    if let Err(e) = dispatch::replan_for_agent(&ctx, &uuid.to_string()).await {
        tracing::warn!(error = %e, "re-planning accepted tasks on poll failed");
    }
    let pending = tasks::Model::find_pending_for_agent(&ctx.db, &uuid.to_string()).await?;

    let mut out = Vec::with_capacity(pending.len());
    for t in pending {
        let plan = t
            .plan
            .as_ref()
            .and_then(|p| serde_json::from_str::<Value>(p).ok())
            .unwrap_or(Value::Null);
        out.push(json!({
            "taskId": t.task_id.to_string(),
            "intent": t.intent.clone(),
            "status": t.status.clone(),
            "autoRollback": plan.get("auto_rollback").and_then(Value::as_bool).unwrap_or(false),
            "plan": plan,
        }));
        // Best-effort: mark dispatched so it isn't treated as freshly planned.
        if let Err(e) = t.mark_dispatched(&ctx.db).await {
            tracing::warn!(error = %e, "failed to mark task dispatched");
        }
    }
    format::json(out)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskResultRequest {
    /// `success` or `failed`.
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    /// The overall exit code (0 = every critical step succeeded). Absent means
    /// 0 for `success` and 1 otherwise.
    #[serde(default)]
    pub exit_code: Option<i64>,
    /// Combined output. Stored capped at [`tasks::OUTPUT_CAP`] bytes (the tail
    /// is kept behind a truncation marker) and passed through to the Hub.
    #[serde(default)]
    pub output: Option<String>,
    /// Per-step results, in plan order.
    #[serde(default)]
    pub steps: Option<Vec<StepResultRequest>>,
}

/// One step's outcome in a result body. Every field is optional so a partial
/// step never rejects the whole result.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepResultRequest {
    #[serde(default, alias = "stepId")]
    pub id: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
    /// `success`, `failed` or `skipped` (the rmm-agent's vocabulary).
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub changed: Option<bool>,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

impl From<StepResultRequest> for tasks::StepResult {
    fn from(s: StepResultRequest) -> Self {
        Self {
            id: s.id.unwrap_or_default(),
            action: s.action.unwrap_or_default(),
            status: s.status.unwrap_or_default(),
            changed: s.changed.unwrap_or(false),
            output: s.output.unwrap_or_default(),
            error: s.error.unwrap_or_default(),
        }
    }
}

/// Fold per-step results into one readable block, for a result that carried
/// steps but no combined `output` (each step's output, or its error, under a
/// header naming the step).
fn steps_output(steps: &[tasks::StepResult]) -> String {
    steps
        .iter()
        .filter(|s| !s.output.is_empty() || !s.error.is_empty())
        .map(|s| {
            let mut block = format!("==> {} [{}]", s.action, s.status);
            if !s.output.is_empty() {
                block.push('\n');
                block.push_str(&s.output);
            }
            if !s.error.is_empty() {
                block.push_str("\nerror: ");
                block.push_str(&s.error);
            }
            block
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `POST /api/v1/agents/{id}/tasks/{task_id}/result` — record a terminal task
/// result reported by the agent (verdict, exit code, capped output and
/// per-step results, served back by `GET /api/v1/tasks/{id}`), surface it in
/// the operational log, and forward it to the Hub.
pub async fn report_result(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path((id, task_id)): Path<(String, String)>,
    Json(req): Json<TaskResultRequest>,
) -> Result<Response> {
    let agent_uuid = parse_uuid(&id)?;
    let caller = system_token::authorize_agent(&ctx, &headers, &agent_uuid).await?;
    let tid = parse_uuid(&task_id)?;

    let task = tasks::Model::find_by_task_id(&ctx.db, &tid).await?;
    // An agent reports only on its own tasks.
    if matches!(caller, Caller::Agent(_)) && !task.targets_agent(&agent_uuid.to_string()) {
        return Err(loco_rs::Error::NotFound);
    }
    let final_status = if req.status == "success" {
        "completed"
    } else {
        "failed"
    };
    let hub_status = if final_status == "failed" {
        "failed"
    } else {
        "success"
    };
    let exit_code = req
        .exit_code
        .unwrap_or(if hub_status == "failed" { 1 } else { 0 });
    let steps: Vec<tasks::StepResult> = req
        .steps
        .unwrap_or_default()
        .into_iter()
        .map(tasks::StepResult::from)
        .collect();
    // What the run printed: the agent's combined output when it sent one,
    // otherwise whatever the steps printed.
    let run_output = req
        .output
        .filter(|o| !o.is_empty())
        .or_else(|| Some(steps_output(&steps)).filter(|o| !o.is_empty()));

    let message = req
        .message
        .clone()
        .unwrap_or_else(|| format!("task {final_status}"));
    let params = tasks::RecordResultParams {
        status: final_status.to_string(),
        result_status: req.status.clone(),
        error: req.error.clone(),
        message: req.message,
        exit_code,
        output: run_output,
        steps,
    };
    let updated = task.record_result(&ctx.db, &params).await?;

    let audit = json!({
        "agent_id": id,
        "task_id": task_id,
        "level": if final_status == "failed" { "error" } else { "info" },
        "source": "agent",
        "message": message,
    });
    if let Err(e) = gateway_client::ship_log(&audit).await {
        tracing::warn!(error = %e, "failed to ship task-result audit log");
    }

    // Close the loop with the Hub: its TaskRun (correlated by this Linexus
    // task id) completes now rather than waiting out the stale-run sweep.
    // Best-effort — the result is already recorded here, and the Hub treats
    // a duplicate delivery as a no-op, so retrying is always safe. The output
    // is the stored (capped) run output when there is one, else the error,
    // else the one-line summary.
    let output = updated
        .output
        .clone()
        .filter(|o| !o.is_empty())
        .or(req.error)
        .unwrap_or_else(|| message.clone());
    if let Err(e) =
        gateway_client::forward_task_result(&task_id, hub_status, exit_code, &output).await
    {
        tracing::warn!(error = %e, task_id = %task_id, "failed to forward result to Daedalus IT");
    }

    format::json(json!({ "taskId": updated.task_id.to_string(), "status": updated.status }))
}

/// The `result` block of a task: what the agent reported, or `null` while the
/// task has not finished. A task finished before results were stored (or by
/// a path that stores none) gets a result derived from its lifecycle status.
fn task_result_json(t: &tasks::Model) -> Value {
    let result_status = match (&t.result_status, t.status.as_str()) {
        (Some(s), _) => s.clone(),
        (None, "completed") => "success".to_string(),
        (None, "failed") => "failed".to_string(),
        (None, _) => return Value::Null,
    };
    let exit_code = t
        .exit_code
        .unwrap_or(if result_status == "success" { 0 } else { 1 });
    let steps: Vec<Value> = t
        .step_results()
        .into_iter()
        .map(|s| {
            json!({
                "id": s.id,
                "action": s.action,
                "status": s.status,
                "changed": s.changed,
                "output": s.output,
                "error": s.error,
            })
        })
        .collect();
    json!({
        "status": result_status,
        "exitCode": exit_code,
        "message": t.result_message.clone().unwrap_or_default(),
        "error": t.error_message.clone().unwrap_or_default(),
        "output": t.output.clone().unwrap_or_default(),
        "steps": steps,
    })
}

/// The `Task` shape served by `GET /api/v1/tasks/{id}`.
fn task_json(t: &tasks::Model) -> Value {
    json!({
        "taskId": t.task_id.to_string(),
        "intent": t.intent,
        "status": t.status,
        "targets": t.targets(),
        "createdAt": t.created_at.to_rfc3339(),
        "updatedAt": t.updated_at.to_rfc3339(),
        "completedAt": t.completed_at.map(|d| d.to_rfc3339()).unwrap_or_default(),
        "result": task_result_json(t),
    })
}

/// `GET /api/v1/tasks/{id}` — one task's lifecycle and, once the agent has
/// reported, its result. This is what a caller polls to follow a task it
/// dispatched to completion.
///
/// `status` is the task lifecycle: `accepted` (recorded, the Orchestrator
/// could not plan it — no agent will pick it up), `planned`, `dispatched` (an
/// agent has been handed the plan), then `completed` or `failed` once the
/// agent reports; `pending` only exists for an instant during creation, and
/// `cancelled` is set by the operator API. An id that names no task — or is
/// not a UUID — is `404 {"error":"not_found"}`.
pub async fn get_task(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response> {
    system_token::authenticate_bearer(&ctx, &headers).await?;
    let not_found = || {
        format::render()
            .status(axum::http::StatusCode::NOT_FOUND)
            .json(json!({ "error": "not_found" }))
    };
    let Ok(tid) = uuid::Uuid::parse_str(&id) else {
        return not_found();
    };
    match tasks::Model::find_by_task_id(&ctx.db, &tid).await {
        Ok(task) => format::json(task_json(&task)),
        Err(ModelError::EntityNotFound) => not_found(),
        Err(e) => Err(e.into()),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShipLogEntry {
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    pub message: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ShipLogBody {
    One(ShipLogEntry),
    Many(Vec<ShipLogEntry>),
}

/// `POST /api/v1/agents/{id}/logs` — the agent ships journal lines; Nexus stamps
/// the agent id and forwards them to the Logger. This is how agent logs reach
/// the audit trail without the agent talking to the Logger directly.
pub async fn ship_agent_logs(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ShipLogBody>,
) -> Result<Response> {
    let uuid = parse_uuid(&id)?;
    system_token::authorize_agent(&ctx, &headers, &uuid).await?;

    let entries = match body {
        ShipLogBody::One(e) => vec![e],
        ShipLogBody::Many(v) => v,
    };
    let mut ingested = 0usize;
    for e in entries {
        let entry = json!({
            "agent_id": uuid.to_string(),
            "task_id": e.task_id,
            "level": e.level.unwrap_or_else(|| "info".to_string()),
            "source": e.source.unwrap_or_else(|| "agent".to_string()),
            "message": e.message,
            "metadata": e.metadata,
        });
        gateway_client::ship_log(&entry)
            .await
            .map_err(|err| loco_rs::Error::Any(err.into()))?;
        ingested += 1;
    }
    format::json(json!({ "ingested": ingested }))
}

pub fn routes() -> Routes {
    Routes::new()
        .prefix("api/v1")
        .add("/agents", get(list_agents))
        .add("/agents/enroll", post(enroll))
        .add("/agents/{id}", get(get_agent))
        .add("/agents/{id}/services", get(agent_services))
        .add("/agents/{id}/packages", get(agent_packages))
        .add("/agents/{id}/report", post(report))
        .add(
            "/agents/{id}/environment",
            get(get_environment).post(set_environment),
        )
        .add("/agents/{id}/heartbeat", post(heartbeat))
        .add("/agents/{id}/logs", get(agent_logs).post(ship_agent_logs))
        .add("/agents/{id}/tasks", get(poll_tasks))
        .add("/agents/{id}/tasks/{task_id}/result", post(report_result))
        .add("/tasks", post(create_task))
        .add("/tasks/{id}", get(get_task))
        .add("/tasks/{id}/cancel", post(cancel_task))
}
