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

    // `set_environment` is the one intent that also changes what this service
    // knows, not just what an agent is asked to do. Applying it here — rather
    // than only on the dedicated endpoint — means the inventory is right no
    // matter which door the request came through, and a caller that dispatches
    // the intent without calling the endpoint cannot leave Nexus believing a
    // machine is something it is not.
    if req.intent == "set_environment" {
        apply_environment_intent(&ctx, &req).await;
    }

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
            // Store just the TransactionPlan (the orchestrator wraps it in a
            // PlanResponse envelope) so the agent poll can hand it over directly.
            let plan_obj = plan.get("plan").cloned().unwrap_or(plan);
            let plan_str = serde_json::to_string(&plan_obj).unwrap_or_default();
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

/// Persist a `set_environment` intent's params onto every agent it targets.
///
/// Targets are matched by agent id first and hostname second, mirroring what
/// Daedalus IT sends (it falls back to the hostname for a machine with no
/// agent id yet). A target that matches nothing is logged and skipped — this
/// runs alongside task creation and must never fail the dispatch, because the
/// task itself is already recorded and the agent will still be told.
async fn apply_environment_intent(ctx: &AppContext, req: &CreateTaskRequest) {
    let environment = req
        .params
        .get("environment")
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| agents::DEFAULT_ENVIRONMENT.to_string());
    // Anything other than an explicit "false" leaves the machine tracked.
    // Going quiet has to be asked for, never inferred from a missing field.
    let monitored = req
        .params
        .get("monitored")
        .map(|v| !matches!(v.trim().to_lowercase().as_str(), "false" | "0" | "no"))
        .unwrap_or(true);
    let note = req.params.get("note").map(|s| s.trim().to_string());

    for target in &req.targets {
        let found = match uuid::Uuid::parse_str(target) {
            Ok(id) => agents::Model::find_by_agent_id(&ctx.db, &id).await.ok(),
            Err(_) => agents::Model::find_by_hostname(&ctx.db, target).await.ok(),
        };
        let Some(agent) = found else {
            tracing::warn!(target = %target, "set_environment: no such agent, inventory not updated");
            continue;
        };
        let params = agents::SetEnvironmentParams {
            environment: environment.clone(),
            monitored,
            note: note.clone(),
        };
        if let Err(e) = agent.set_environment(&ctx.db, &params).await {
            tracing::warn!(error = %e, target = %target, "set_environment: inventory update failed");
        }
    }
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
    system_token::authenticate_bearer(&ctx, &headers).await?;
    let uuid = parse_uuid(&id)?;
    let agent = agents::Model::find_by_agent_id(&ctx.db, &uuid).await?;
    format::json(environment_json(&agent))
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

// ---------------------------------------------------------------------------
// Agent task loop: poll for work, report results, ship logs (all via Nexus, so
// the agent never talks to the Orchestrator or Logger directly).
// ---------------------------------------------------------------------------

/// `GET /api/v1/agents/{id}/tasks` — planned/dispatched tasks targeting this
/// agent, each with its TransactionPlan. Handing a plan over transitions the
/// task from `planned` to `dispatched`.
pub async fn poll_tasks(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response> {
    system_token::authenticate_bearer(&ctx, &headers).await?;
    let uuid = parse_uuid(&id)?;
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
    system_token::authenticate_bearer(&ctx, &headers).await?;
    parse_uuid(&id)?;
    let tid = parse_uuid(&task_id)?;

    let task = tasks::Model::find_by_task_id(&ctx.db, &tid).await?;
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
    system_token::authenticate_bearer(&ctx, &headers).await?;
    let uuid = parse_uuid(&id)?;

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
}
