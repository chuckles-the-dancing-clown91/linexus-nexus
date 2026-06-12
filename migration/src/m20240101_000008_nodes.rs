use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(
            m,
            "nodes",
            &[
                ("id", ColType::PkAuto),
                ("node_id", ColType::Uuid),
                // human | infra_generator | infra_consumer | council
                ("class", ColType::String),
                ("label", ColType::String),
                ("status", ColType::String),
                // childhood | learning | labor | rehabilitation | retirement
                ("lifecycle_phase", ColType::StringNull),
                ("capabilities", ColType::TextNull),
                ("public_key", ColType::StringNull),
                // DB id of the owning Nexus user, when the node fronts an account.
                ("owner_user_id", ColType::IntegerNull),
                // Originating service, e.g. "tea-and-madness".
                ("source", ColType::StringNull),
                // External handle/pid in the originating service.
                ("external_ref", ColType::StringNull),
                ("commissioned_at", ColType::TimestampWithTimeZoneNull),
            ],
            &[],
        )
        .await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "nodes").await?;
        Ok(())
    }
}
