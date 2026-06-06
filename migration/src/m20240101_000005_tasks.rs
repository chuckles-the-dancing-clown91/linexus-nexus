use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(
            m,
            "tasks",
            &[
                ("id", ColType::PkAuto),
                ("task_id", ColType::Uuid),
                ("intent", ColType::Text),
                ("status", ColType::String),
                ("created_by", ColType::String),
                ("target_agents", ColType::TextNull),
                ("signed_envelope", ColType::TextNull),
                ("completed_at", ColType::TimestampWithTimeZoneNull),
                ("error_message", ColType::TextNull),
            ],
            &[],
        )
        .await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "tasks").await?;
        Ok(())
    }
}
