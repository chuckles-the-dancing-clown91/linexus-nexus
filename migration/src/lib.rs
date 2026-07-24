#![allow(elided_lifetimes_in_paths)]
#![allow(clippy::wildcard_imports)]
// Mirror the main crate (src/lib.rs): pedantic/nursery are opinionated style
// lints the codebase does not adopt wholesale, so allow them centrally here too
// and keep the enforced surface at the default `clippy::all` set.
#![allow(clippy::pedantic)]
#![allow(clippy::nursery)]
pub use sea_orm_migration::prelude::*;
mod m20220101_000001_users;
mod m20240101_000002_roles;
mod m20240101_000003_user_roles;
mod m20240101_000004_agents;
mod m20240101_000005_tasks;
mod m20240101_000006_casbin_rules;
mod m20240101_000007_add_plan_to_users;
mod m20240101_000008_nodes;
mod m20240101_000009_demiurge_lots;
mod m20240101_000010_contributions;
mod m20240101_000011_payments;
mod m20240101_000012_system_tokens;
mod m20240101_000013_support_sessions;
mod m20240101_000014_housing_nodes;
mod m20240101_000015_housing_units;
mod m20240101_000016_housing_council_reviews;
mod m20240101_000017_housing_occupancies;
mod m20240101_000018_housing_queue;
mod m20240101_000019_housing_maintenance;
mod m20240101_000020_housing_documents;
mod m20240101_000021_agent_facts;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20220101_000001_users::Migration),
            Box::new(m20240101_000002_roles::Migration),
            Box::new(m20240101_000003_user_roles::Migration),
            Box::new(m20240101_000004_agents::Migration),
            Box::new(m20240101_000005_tasks::Migration),
            Box::new(m20240101_000006_casbin_rules::Migration),
            Box::new(m20240101_000007_add_plan_to_users::Migration),
            Box::new(m20240101_000008_nodes::Migration),
            Box::new(m20240101_000009_demiurge_lots::Migration),
            Box::new(m20240101_000010_contributions::Migration),
            Box::new(m20240101_000011_payments::Migration),
            Box::new(m20240101_000012_system_tokens::Migration),
            Box::new(m20240101_000013_support_sessions::Migration),
            Box::new(m20240101_000014_housing_nodes::Migration),
            Box::new(m20240101_000015_housing_units::Migration),
            Box::new(m20240101_000016_housing_council_reviews::Migration),
            Box::new(m20240101_000017_housing_occupancies::Migration),
            Box::new(m20240101_000018_housing_queue::Migration),
            Box::new(m20240101_000019_housing_maintenance::Migration),
            Box::new(m20240101_000020_housing_documents::Migration),
            Box::new(m20240101_000021_agent_facts::Migration),
            // inject-above (do not remove this comment)
        ]
    }
}
