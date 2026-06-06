use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(
            m,
            "casbin_rules",
            &[
                ("id", ColType::PkAuto),
                ("ptype", ColType::String),
                ("v0", ColType::StringNull),
                ("v1", ColType::StringNull),
                ("v2", ColType::StringNull),
                ("v3", ColType::StringNull),
                ("v4", ColType::StringNull),
                ("v5", ColType::StringNull),
            ],
            &[],
        )
        .await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "casbin_rules").await?;
        Ok(())
    }
}
