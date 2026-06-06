use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(
            m,
            "roles",
            &[
                ("id", ColType::PkAuto),
                ("name", ColType::StringUniq),
                ("description", ColType::StringNull),
                ("permissions", ColType::Text),
                ("is_system", ColType::Boolean),
            ],
            &[],
        )
        .await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "roles").await?;
        Ok(())
    }
}
