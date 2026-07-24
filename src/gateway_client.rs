//! Outbound HTTP clients for the two services Nexus fronts on the RMM path:
//! the Orchestrator (planning) and the Logger (audit trail).
//!
//! Daedalus IT only ever talks to Nexus; Nexus fans out to these. Endpoints and
//! the shared service token are read from the environment:
//!   * `LINEXUS_ORCH_URL`   (default `http://127.0.0.1:5152`)
//!   * `LINEXUS_LOGGER_URL` (default `http://127.0.0.1:5151`)
//!   * `LINEXUS_SERVICE_TOKEN` — bearer token presented to both.
//!
//! Proxies are explicitly disabled: these are internal calls that must not be
//! routed through an outbound web proxy.

use std::time::Duration;

use serde_json::Value;

fn orch_url() -> String {
    std::env::var("LINEXUS_ORCH_URL").unwrap_or_else(|_| "http://127.0.0.1:5152".to_string())
}

fn logger_url() -> String {
    std::env::var("LINEXUS_LOGGER_URL").unwrap_or_else(|_| "http://127.0.0.1:5151".to_string())
}

fn service_token() -> Option<String> {
    std::env::var("LINEXUS_SERVICE_TOKEN")
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
/// returns the orchestrator's `PlanResponse` as JSON.
pub async fn plan_task(body: &Value) -> anyhow::Result<Value> {
    let url = format!("{}/plan", orch_url());
    let resp = with_auth(client()?.post(&url).json(body))
        .send()
        .await?
        .error_for_status()?;
    Ok(resp.json().await?)
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
    Ok(())
}
