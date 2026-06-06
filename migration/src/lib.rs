#![allow(elided_lifetimes_in_paths)]
#![allow(clippy::wildcard_imports)]
pub use sea_orm_migration::prelude::*;
mod m20220101_000001_users;
mod m20240101_000002_roles;
mod m20240101_000003_user_roles;
mod m20240101_000004_agents;
mod m20240101_000005_tasks;
mod m20240101_000006_casbin_rules;

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
            // inject-above (do not remove this comment)
        ]
    }
}
