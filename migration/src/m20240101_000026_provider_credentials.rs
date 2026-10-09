//! Stored provider credentials and the last known state of each provider.
//!
//! `sealed_token` is the API token sealed with AES-256-GCM under
//! `NEXUS_SECRET_KEY` (12-byte nonce prepended); it is never readable back
//! through the API. A row can exist without a token: it then only carries the
//! status of a provider configured from the environment.

use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(
            m,
            "provider_credentials",
            &[
                ("id", ColType::PkAuto),
                ("provider", ColType::StringUniq),
                ("sealed_token", ColType::BlobNull),
                ("account_id", ColType::StringNull),
                ("account_name", ColType::StringNull),
                ("state", ColType::StringNull),
                ("detail", ColType::TextNull),
                ("checked_at", ColType::TimestampWithTimeZoneNull),
            ],
            &[],
        )
        .await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "provider_credentials").await
    }
}
