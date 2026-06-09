use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Service-to-service credentials. A token is stored only as its SHA-256 hash;
/// the plaintext is shown once at issuance and never persisted. This is how the
/// Tea & Madness publisher (and any other trusted service) authenticates to the
/// Nexus to create nodes and route payments.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(
            m,
            "system_tokens",
            &[
                ("id", ColType::PkAuto),
                ("token_id", ColType::Uuid),
                ("service", ColType::String),
                ("token_hash", ColType::StringUniq),
                // Comma-separated scope grants; "*" grants all.
                ("scopes", ColType::StringNull),
                ("active", ColType::Boolean),
                ("last_used_at", ColType::TimestampWithTimeZoneNull),
            ],
            &[],
        )
        .await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "system_tokens").await?;
        Ok(())
    }
}
