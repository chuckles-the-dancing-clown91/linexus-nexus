//! Background sweep that re-plans `accepted` tasks every 60 s (see
//! [`crate::dispatch::sweep_accepted`]). It is started only when the server
//! sets up its routes, and never in the test environment — tests call the
//! sweep function directly.

use async_trait::async_trait;
use axum::Router as AxumRouter;
use loco_rs::{
    app::{AppContext, Initializer},
    environment::Environment,
    Result,
};

use crate::dispatch;

/// How often the sweep runs.
pub const INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

pub struct TaskSweep;

#[async_trait]
impl Initializer for TaskSweep {
    fn name(&self) -> String {
        "task-sweep".to_string()
    }

    async fn after_routes(&self, router: AxumRouter, ctx: &AppContext) -> Result<AxumRouter> {
        if matches!(ctx.environment, Environment::Test) {
            return Ok(router);
        }
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(INTERVAL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                match dispatch::sweep_accepted(&ctx).await {
                    Ok(r) if r.planned + r.expired > 0 => tracing::info!(
                        planned = r.planned,
                        expired = r.expired,
                        still_accepted = r.still_accepted,
                        "accepted-task sweep"
                    ),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "accepted-task sweep failed"),
                }
            }
        });
        Ok(router)
    }
}
