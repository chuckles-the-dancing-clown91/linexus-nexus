//! BIND zones served by our own agents. Nexus is the source of truth for
//! them: every change bumps `serial`, re-renders the zone file and dispatches
//! `dns_zone_apply` to the primary and each secondary. `task_ids` is the JSON
//! list of the tasks sent for the latest change (the primary's first), from
//! which `applyStatus` is derived.

use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(
            m,
            "dns_zones",
            &[
                ("id", ColType::PkAuto),
                ("zone_id", ColType::UuidUniq),
                ("name", ColType::StringUniq),
                ("primary_agent_id", ColType::Uuid),
                ("secondary_agent_ids", ColType::TextNull),
                ("default_ttl", ColType::Integer),
                ("admin_email", ColType::String),
                ("serial", ColType::BigInteger),
                ("last_task_id", ColType::StringNull),
                ("task_ids", ColType::TextNull),
            ],
            &[],
        )
        .await?;
        create_table(
            m,
            "dns_records",
            &[
                ("id", ColType::PkAuto),
                ("record_id", ColType::UuidUniq),
                ("zone_id", ColType::Uuid),
                ("record_type", ColType::String),
                ("name", ColType::String),
                ("content", ColType::Text),
                ("ttl", ColType::Integer),
                ("priority", ColType::IntegerNull),
                ("comment", ColType::TextNull),
            ],
            &[],
        )
        .await?;
        m.create_index(
            Index::create()
                .name("idx_dns_records_zone_id")
                .table(Alias::new("dns_records"))
                .col(Alias::new("zone_id"))
                .to_owned(),
        )
        .await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "dns_records").await?;
        drop_table(m, "dns_zones").await
    }
}
