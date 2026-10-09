//! The provider operations log: one row per mutation Nexus performed (or
//! tried to) against DigitalOcean, Cloudflare or a BIND zone. A row that
//! succeeded with an `idempotency_key` also keeps the `response` it answered,
//! so the same key within 24 h replays it instead of acting twice.

use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(
            m,
            "provider_operations",
            &[
                ("id", ColType::PkAuto),
                ("operation_id", ColType::UuidUniq),
                ("provider", ColType::String),
                ("operation", ColType::String),
                ("target", ColType::TextNull),
                ("requester", ColType::TextNull),
                ("status", ColType::String),
                ("error", ColType::TextNull),
                ("idempotency_key", ColType::StringNull),
                ("response_status", ColType::IntegerNull),
                ("response", ColType::TextNull),
            ],
            &[],
        )
        .await?;
        m.create_index(
            Index::create()
                .name("idx_provider_operations_idempotency_key")
                .table(Alias::new("provider_operations"))
                .col(Alias::new("idempotency_key"))
                .to_owned(),
        )
        .await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "provider_operations").await
    }
}
