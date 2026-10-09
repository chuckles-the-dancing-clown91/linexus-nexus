//! Task dispatch: the one path every task takes on its way to an agent.
//!
//! `POST /api/v1/tasks` and every intent Nexus raises itself (BIND zone
//! applies, DNS server installs, volume mounts) go through [`dispatch`]:
//! resolve `hostgroup:<name>` targets, insert the task, ask the Orchestrator
//! for a plan (`POST /plan`) and store it. When the Orchestrator cannot be
//! reached the task is kept as `accepted` with its plan request, and
//! [`replan_for_agent`] (on an agent's poll) or [`sweep_accepted`] (every
//! 60 s in the server) plans it later — for up to [`REPLAN_WINDOW_HOURS`],
//! after which it is `failed` with `result.error = "never planned"`.

use std::collections::BTreeMap;

use loco_rs::app::AppContext;
use loco_rs::model::{ModelError, ModelResult};
use serde_json::{json, Value};

use crate::gateway_client;
use crate::models::{agents, tasks};

/// How long an `accepted` task keeps being re-planned.
pub const REPLAN_WINDOW_HOURS: i64 = 24;
/// Target prefix that expands to every agent of a hostgroup.
pub const HOSTGROUP_PREFIX: &str = "hostgroup:";

/// What to dispatch.
#[derive(Debug, Clone, Default)]
pub struct DispatchRequest {
    pub intent: String,
    /// Agent ids, hostnames, or `hostgroup:<name>` entries.
    pub targets: Vec<String>,
    /// Recorded as the task's creator and sent as `requester_id`.
    pub requester: String,
    pub auto_rollback: bool,
    pub params: BTreeMap<String, String>,
}

/// Expand `hostgroup:<name>` entries to the ids of every agent in that
/// hostgroup (at this moment), keeping other entries as given, without
/// duplicates and in order.
pub async fn resolve_targets(
    db: &sea_orm::DatabaseConnection,
    targets: &[String],
) -> ModelResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for t in targets {
        let t = t.trim();
        if let Some(group) = t.strip_prefix(HOSTGROUP_PREFIX) {
            for a in agents::Model::find_by_hostgroup(db, group.trim()).await? {
                let id = a.agent_id.to_string();
                if !out.contains(&id) {
                    out.push(id);
                }
            }
        } else if !t.is_empty() && !out.iter().any(|o| o == t) {
            out.push(t.to_string());
        }
    }
    Ok(out)
}

/// Record a task, have the Orchestrator plan it, and store the plan. The
/// returned task is `planned`, or `accepted` when the Orchestrator could not
/// be reached (it is re-planned later). Targets must already be resolved
/// unless they contain `hostgroup:` entries, which are expanded here.
pub async fn dispatch(ctx: &AppContext, req: &DispatchRequest) -> ModelResult<tasks::Model> {
    let targets = resolve_targets(&ctx.db, &req.targets).await?;
    let params = tasks::CreateTaskParams {
        intent: req.intent.clone(),
        target_agents: Some(targets.clone()),
    };
    let task = tasks::Model::create(&ctx.db, &req.requester, &params).await?;

    // `set_environment` is the one intent that also changes what this service
    // knows, not just what an agent is asked to do. Applying it here means the
    // inventory is right no matter which door the request came through.
    if req.intent == "set_environment" {
        apply_environment_intent(ctx, &targets, &req.params).await;
    }

    let plan_body = json!({
        "intent": req.intent,
        "targets": targets,
        "requester_id": req.requester,
        "auto_rollback": req.auto_rollback,
        "params": req.params,
        "task_id": task.task_id.to_string(),
    });
    let task = task
        .set_plan_request(&ctx.db, &plan_body.to_string())
        .await?;

    let planned = plan(&task, &plan_body).await;
    let (to, plan_str) = match &planned {
        Ok(p) => ("planned", Some(p.as_str())),
        Err(e) => {
            tracing::warn!(error = %e, task_id = %task.task_id, "orchestrator planning failed; task recorded unplanned (accepted)");
            ("accepted", None)
        }
    };
    tasks::Model::transition(&ctx.db, &task.task_id, &["pending"], to, plan_str).await?;
    let task = tasks::Model::find_by_task_id(&ctx.db, &task.task_id).await?;

    // Best-effort: surface the task in the operational log so it appears when
    // the Hub tails the journal. A logging failure never fails the request.
    let audit = json!({
        "task_id": task.task_id.to_string(),
        "level": "info",
        "source": "nexus",
        "message": format!("task planned: {}", req.intent),
        "metadata": {
            "intent": req.intent,
            "targets": targets,
            "requester": req.requester,
            "status": task.status,
        },
    });
    if let Err(e) = gateway_client::ship_log(&audit).await {
        tracing::warn!(error = %e, "failed to ship task audit log");
    }
    Ok(task)
}

/// Call the Orchestrator and return the TransactionPlan to store (the
/// Orchestrator wraps it in a `PlanResponse` envelope; the agent poll hands
/// the bare plan over).
async fn plan(task: &tasks::Model, body: &Value) -> anyhow::Result<String> {
    let resp = gateway_client::plan_task(body).await?;
    let plan_obj = resp.get("plan").cloned().unwrap_or(resp);
    if !plan_obj.is_object() {
        anyhow::bail!(
            "orchestrator answered a plan that is not an object for {}",
            task.task_id
        );
    }
    Ok(serde_json::to_string(&plan_obj)?)
}

/// The plan request stored on `task`, or one rebuilt from what the row
/// knows (tasks recorded before plan requests were kept).
fn plan_request_of(task: &tasks::Model) -> Value {
    task.plan_request
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or_else(|| {
            json!({
                "intent": task.intent,
                "targets": task.targets(),
                "requester_id": task.created_by,
                "auto_rollback": false,
                "params": {},
                "task_id": task.task_id.to_string(),
            })
        })
}

/// Outcome of trying to plan one `accepted` task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Replan {
    Planned,
    Expired,
    StillAccepted,
}

/// Try once to plan an `accepted` task, or give up on it past the window.
pub async fn replan(ctx: &AppContext, task: &tasks::Model) -> ModelResult<Replan> {
    let age = chrono::Utc::now().signed_duration_since(task.created_at.with_timezone(&chrono::Utc));
    if age > chrono::Duration::hours(REPLAN_WINDOW_HOURS) {
        tasks::Model::fail_never_planned(&ctx.db, &task.task_id).await?;
        tracing::warn!(task_id = %task.task_id, "task never planned; marked failed");
        return Ok(Replan::Expired);
    }
    match plan(task, &plan_request_of(task)).await {
        Ok(p) => {
            let moved = tasks::Model::transition(
                &ctx.db,
                &task.task_id,
                &["accepted"],
                "planned",
                Some(&p),
            )
            .await?;
            Ok(if moved {
                Replan::Planned
            } else {
                Replan::StillAccepted
            })
        }
        Err(e) => {
            tracing::debug!(error = %e, task_id = %task.task_id, "re-plan failed");
            Ok(Replan::StillAccepted)
        }
    }
}

/// Re-plan the `accepted` tasks that target `agent_id` (called when that
/// agent polls, so it gets them on this very poll when the Orchestrator is
/// back).
pub async fn replan_for_agent(ctx: &AppContext, agent_id: &str) -> ModelResult<()> {
    for task in tasks::Model::find_accepted(&ctx.db).await? {
        if task.targets_agent(agent_id) && replan(ctx, &task).await? == Replan::StillAccepted {
            // The Orchestrator is still down; don't hold the poll up further.
            break;
        }
    }
    Ok(())
}

/// What one sweep did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub planned: usize,
    pub expired: usize,
    pub still_accepted: usize,
}

/// Re-plan every `accepted` task once (expiring the ones past the window).
/// Stops calling the Orchestrator after the first failure — it is down — but
/// still expires what is past the window. The server runs this every 60 s;
/// tests call it directly.
pub async fn sweep_accepted(ctx: &AppContext) -> ModelResult<SweepReport> {
    let mut report = SweepReport::default();
    let mut orchestrator_down = false;
    let cutoff = chrono::Utc::now() - chrono::Duration::hours(REPLAN_WINDOW_HOURS);
    for task in tasks::Model::find_accepted(&ctx.db).await? {
        let expired = task.created_at.with_timezone(&chrono::Utc) < cutoff;
        if orchestrator_down && !expired {
            report.still_accepted += 1;
            continue;
        }
        match replan(ctx, &task).await? {
            Replan::Planned => report.planned += 1,
            Replan::Expired => report.expired += 1,
            Replan::StillAccepted => {
                report.still_accepted += 1;
                orchestrator_down = true;
            }
        }
    }
    Ok(report)
}

/// Cancel a task. `Ok(false)` when it is already terminal.
pub async fn cancel(ctx: &AppContext, task_id: &uuid::Uuid) -> ModelResult<bool> {
    let task = tasks::Model::find_by_task_id(&ctx.db, task_id).await?;
    if task.is_terminal() {
        return Ok(false);
    }
    let moved = tasks::Model::transition(
        &ctx.db,
        task_id,
        &["pending", "accepted", "planned", "dispatched"],
        "cancelled",
        None,
    )
    .await?;
    if !moved {
        // Raced to a terminal state.
        let again = tasks::Model::find_by_task_id(&ctx.db, task_id).await?;
        if again.is_terminal() {
            return Ok(false);
        }
        return Err(ModelError::Message("task changed while cancelling".into()));
    }
    Ok(true)
}

/// Persist a `set_environment` intent's params onto every agent it targets.
///
/// Targets are matched by agent id first and hostname second, mirroring what
/// the Hub sends. A target that matches nothing is logged and skipped — this
/// runs alongside task creation and must never fail the dispatch.
async fn apply_environment_intent(
    ctx: &AppContext,
    targets: &[String],
    params: &BTreeMap<String, String>,
) {
    let environment = params
        .get("environment")
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| agents::DEFAULT_ENVIRONMENT.to_string());
    // Anything other than an explicit "false" leaves the machine tracked.
    let monitored = params
        .get("monitored")
        .is_none_or(|v| !matches!(v.trim().to_lowercase().as_str(), "false" | "0" | "no"));
    let note = params.get("note").map(|s| s.trim().to_string());

    for target in targets {
        let found = match uuid::Uuid::parse_str(target) {
            Ok(id) => agents::Model::find_by_agent_id(&ctx.db, &id).await.ok(),
            Err(_) => agents::Model::find_by_hostname(&ctx.db, target).await.ok(),
        };
        let Some(agent) = found else {
            tracing::warn!(target = %target, "set_environment: no such agent, inventory not updated");
            continue;
        };
        let p = agents::SetEnvironmentParams {
            environment: environment.clone(),
            monitored,
            note: note.clone(),
        };
        if let Err(e) = agent.set_environment(&ctx.db, &p).await {
            tracing::warn!(error = %e, target = %target, "set_environment: inventory update failed");
        }
    }
}
