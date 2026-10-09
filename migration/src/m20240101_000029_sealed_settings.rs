//! Small named secrets Nexus keeps for itself (today: the Ed25519 seed it
//! signs agent plans with). `sealed_value` is sealed with AES-256-GCM under
//! `NEXUS_SECRET_KEY`, with `name` bound in as associated data.

use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(
            m,
            "sealed_settings",
            &[
                ("id", ColType::PkAuto),
                ("name", ColType::StringUniq),
                ("sealed_value", ColType::Blob),
            ],
            &[],
        )
        .await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "sealed_settings").await
    }
}
