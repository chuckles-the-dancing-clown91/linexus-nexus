//! A few process counters, served on `GET /metrics` in Prometheus text format
//! ([`crate::controllers::metrics`]).
//!
//! No metrics crate: the counters are static atomics and the exporter is a
//! handful of lines. They count what the RMM path does — requests, tasks
//! through their lifecycle, logs relayed — not the economy line.

use std::sync::atomic::{AtomicU64, Ordering};

use axum::{extract::Request, middleware::Next, response::Response};

static HTTP_REQUESTS: AtomicU64 = AtomicU64::new(0);
static TASKS_CREATED: AtomicU64 = AtomicU64::new(0);
static TASKS_PLANNED: AtomicU64 = AtomicU64::new(0);
static TASKS_PLAN_REJECTED: AtomicU64 = AtomicU64::new(0);
static TASKS_COMPLETED: AtomicU64 = AtomicU64::new(0);
static TASKS_FAILED: AtomicU64 = AtomicU64::new(0);
static LOGS_SHIPPED: AtomicU64 = AtomicU64::new(0);

pub fn task_created() {
    TASKS_CREATED.fetch_add(1, Ordering::Relaxed);
}
pub fn task_planned() {
    TASKS_PLANNED.fetch_add(1, Ordering::Relaxed);
}
/// The Orchestrator refused to plan an intent.
pub fn task_plan_rejected() {
    TASKS_PLAN_REJECTED.fetch_add(1, Ordering::Relaxed);
}
/// An agent reported a terminal result; `ok` is its verdict.
pub fn task_finished(ok: bool) {
    if ok {
        TASKS_COMPLETED.fetch_add(1, Ordering::Relaxed);
    } else {
        TASKS_FAILED.fetch_add(1, Ordering::Relaxed);
    }
}
/// One event relayed to the Logger.
pub fn log_shipped() {
    LOGS_SHIPPED.fetch_add(1, Ordering::Relaxed);
}

/// Middleware counting every request (including `/metrics` itself).
pub async fn count_request(req: Request, next: Next) -> Response {
    HTTP_REQUESTS.fetch_add(1, Ordering::Relaxed);
    next.run(req).await
}

/// The exposition, `text/plain; version=0.0.4`.
#[must_use]
pub fn render() -> String {
    let mut out = String::new();
    for (name, help, value) in [
        (
            "nexus_http_requests_total",
            "HTTP requests received.",
            HTTP_REQUESTS.load(Ordering::Relaxed),
        ),
        (
            "nexus_tasks_created_total",
            "Tasks recorded.",
            TASKS_CREATED.load(Ordering::Relaxed),
        ),
        (
            "nexus_tasks_planned_total",
            "Tasks the Orchestrator planned (at creation or by re-plan).",
            TASKS_PLANNED.load(Ordering::Relaxed),
        ),
        (
            "nexus_tasks_plan_rejected_total",
            "Tasks failed because the Orchestrator rejected the intent.",
            TASKS_PLAN_REJECTED.load(Ordering::Relaxed),
        ),
        (
            "nexus_tasks_completed_total",
            "Tasks an agent reported as success.",
            TASKS_COMPLETED.load(Ordering::Relaxed),
        ),
        (
            "nexus_tasks_failed_total",
            "Tasks an agent reported as failed.",
            TASKS_FAILED.load(Ordering::Relaxed),
        ),
        (
            "nexus_logs_shipped_total",
            "Audit events relayed to the Logger.",
            LOGS_SHIPPED.load(Ordering::Relaxed),
        ),
    ] {
        out.push_str(&format!(
            "# HELP {name} {help}\n# TYPE {name} counter\n{name} {value}\n"
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_prometheus_text() {
        task_created();
        task_finished(false);
        let text = render();
        assert!(text.contains("# TYPE nexus_tasks_created_total counter\n"));
        let failed: u64 = text
            .lines()
            .find_map(|l| l.strip_prefix("nexus_tasks_failed_total "))
            .unwrap()
            .parse()
            .unwrap();
        assert!(failed >= 1);
    }
}
