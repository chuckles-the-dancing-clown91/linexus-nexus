use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(
            m,
            "agents",
            &[
                ("id", ColType::PkAuto),
                ("agent_id", ColType::Uuid),
                ("hostname", ColType::String),
                ("status", ColType::String),
                ("capability_manifest", ColType::TextNull),
                ("enrolled_at", ColType::TimestampWithTimeZoneNull),
                ("last_heartbeat_at", ColType::TimestampWithTimeZoneNull),
            ],
            &[],
        )
        .await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "agents").await?;
        Ok(())
    }
}
