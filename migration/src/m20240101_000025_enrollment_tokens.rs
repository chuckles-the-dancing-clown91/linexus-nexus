//! One-time (or few-time) enrollment tokens.
//!
//! The Hub mints one per install command, bound to a hostgroup (the client's
//! slug), an environment and its own ids (`metadata`). The plaintext `nxe_…`
//! is returned once; only its SHA-256 is stored. `agent_ids` is the JSON list
//! of every agent that enrolled with it, in order.

use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(
            m,
            "enrollment_tokens",
            &[
                ("id", ColType::PkAuto),
                ("token_id", ColType::UuidUniq),
                ("token_hash", ColType::StringUniq),
                ("hostgroup", ColType::String),
                ("environment", ColType::String),
                ("label", ColType::TextNull),
                ("metadata", ColType::TextNull),
                ("expires_at", ColType::TimestampWithTimeZone),
                ("max_uses", ColType::Integer),
                ("uses", ColType::Integer),
                ("agent_ids", ColType::TextNull),
                ("revoked_at", ColType::TimestampWithTimeZoneNull),
                ("created_by", ColType::StringNull),
            ],
            &[],
        )
        .await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "enrollment_tokens").await
    }
}
