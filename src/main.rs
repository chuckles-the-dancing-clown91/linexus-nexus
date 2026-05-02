//! # Linexus Nexus — The Control Plane

mod governance;
mod middleware;

use tracing_subscriber;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_target(true).with_level(true).init();
    tracing::info!("=== LINEXUS NEXUS — Control Plane ===");
    tracing::info!("Permission Funnel: ACTIVE");
    tracing::info!("Vicinagora Governance: ACTIVE");

    use linexus_core::identity::LifecyclePhase;
    let phase = LifecyclePhase::Labor;
    let (labor, learning) = phase.weekly_obligations();
    tracing::info!("System check — Labor: {} hrs, Learning: {} hrs", labor, learning);
}
