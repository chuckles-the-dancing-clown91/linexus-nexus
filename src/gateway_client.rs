//! Outbound HTTP clients for the two services Nexus fronts on the RMM path:
//! the Orchestrator (planning) and the Logger (audit trail).
//!
//! Daedalus IT only ever talks to Nexus; Nexus fans out to these. Endpoints and
//! the shared service token are read from the environment:
//!   * `LINEXUS_ORCH_URL`   (default `http://127.0.0.1:5152`)
//!   * `LINEXUS_LOGGER_URL` (default `http://127.0.0.1:5151`)
//!   * `LINEXUS_SERVICE_TOKEN` — bearer token presented to both (a production
//!     boot refuses to start without it once either URL is configured, see
//!     [`crate::boot_checks`]).
//!
//! One call goes the other way: task results are forwarded to Daedalus IT's
//! automation results ingest, so the Hub's TaskRuns complete in real time
//! instead of waiting out its stale-run sweep:
//!   * `DAEDALUS_INGEST_URL` — the Hub's API origin (unset = don't forward)
//!   * `DAEDALUS_INGEST_KEY` — its `AUTOMATION_INGEST_KEY`, sent as
//!     `X-Daedalus-Automation-Key`.
//!
//! Proxies are explicitly disabled: these are internal calls that must not be
//! routed through an outbound web proxy.

use std::time::Duration;

use reqwest::StatusCode;
use serde_json::Value;

use crate::metrics;

pub const ENV_ORCH_URL: &str = "LINEXUS_ORCH_URL";
pub const ENV_LOGGER_URL: &str = "LINEXUS_LOGGER_URL";
pub const ENV_SERVICE_TOKEN: &str = "LINEXUS_SERVICE_TOKEN";
pub const ENV_INGEST_URL: &str = "DAEDALUS_INGEST_URL";
pub const ENV_INGEST_KEY: &str = "DAEDALUS_INGEST_KEY";

/// The Orchestrator refused to plan an intent (`400`: unknown intent, missing
/// param). Unlike an unreachable Orchestrator this will not get better on a
/// retry, so the task is failed with the reason instead of kept `accepted`.
#[derive(Debug, Clone)]
pub struct PlanRejected(pub String);

impl std::fmt::Display for PlanRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PlanRejected {}

fn orch_url() -> String {
    std::env::var(ENV_ORCH_URL)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:5152".to_string())
}

fn logger_url() -> String {
    std::env::var(ENV_LOGGER_URL)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:5151".to_string())
}

fn service_token() -> Option<String> {
    std::env::var(ENV_SERVICE_TOKEN)
        .ok()
        .filter(|s| !s.is_empty())
}

fn client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .no_proxy()
        .build()
        .map_err(Into::into)
}

fn with_auth(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    match service_token() {
        Some(t) => req.bearer_auth(t),
        None => req,
    }
}

/// Ask the orchestrator to plan an intent. `body` is the plan request JSON;
/// returns the orchestrator's `PlanResponse` as JSON. A `400` is a
/// [`PlanRejected`] carrying the orchestrator's `error`; anything else that
/// fails is a transport or server error the caller may retry.
pub async fn plan_task(body: &Value) -> anyhow::Result<Value> {
    let url = format!("{}/plan", orch_url());
    let resp = with_auth(client()?.post(&url).json(body)).send().await?;
    if matches!(
        resp.status(),
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
    ) {
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        let reason = body
            .get("error")
            .and_then(Value::as_str)
            .map_or_else(|| format!("orchestrator answered {status}"), str::to_string);
        return Err(PlanRejected(reason).into());
    }
    Ok(resp.error_for_status()?.json().await?)
}

/// Fetch an agent's operational logs from the Logger, most-recent-first.
/// `agent_id` must already be a validated UUID (URL-safe).
pub async fn fetch_agent_logs(agent_id: &str, limit: i64) -> anyhow::Result<Vec<Value>> {
    let url = format!(
        "{}/logs?agent_id={}&limit={}",
        logger_url(),
        agent_id,
        limit
    );
    let resp = with_auth(client()?.get(&url))
        .send()
        .await?
        .error_for_status()?;
    Ok(resp.json().await?)
}

/// Best-effort ingest of one operational event into the Logger. Errors are
/// returned so the caller can log-and-ignore; a logging failure must never fail
/// the originating request.
pub async fn ship_log(entry: &Value) -> anyhow::Result<()> {
    let url = format!("{}/logs", logger_url());
    with_auth(client()?.post(&url).json(entry))
        .send()
        .await?
        .error_for_status()?;
    metrics::log_shipped();
    Ok(())
}

/// Forward a terminal task result to Daedalus IT's automation ingest,
/// correlated by the Linexus task id. Best-effort by contract: the result is
/// already recorded in Nexus before this is attempted, and the Hub's stale-run
/// sweep is the backstop when the Hub is unreachable — so an error here is
/// log-and-continue, never a failure of the agent's report.
pub async fn forward_task_result(
    task_id: &str,
    status: &str,
    exit_code: i64,
    output: &str,
) -> anyhow::Result<()> {
    let Some(base) = std::env::var(ENV_INGEST_URL).ok().filter(|s| !s.is_empty()) else {
        return Ok(()); // forwarding not configured — nothing to do
    };
    let url = format!(
        "{}/api/v1/automation/results/task",
        base.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "linexusTaskId": task_id,
        "status": status,
        "exitCode": exit_code,
        "output": output,
    });
    let mut req = client()?.post(&url).json(&body);
    if let Ok(key) = std::env::var(ENV_INGEST_KEY) {
        if !key.is_empty() {
            req = req.header("X-Daedalus-Automation-Key", key);
        }
    }
    req.send().await?.error_for_status()?;
    Ok(())
}
