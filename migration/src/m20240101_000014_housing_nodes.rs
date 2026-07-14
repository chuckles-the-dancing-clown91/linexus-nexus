use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(
            m,
            "housing_nodes",
            &[
                ("id", ColType::PkAuto),
                // UUID of the backing `nodes` record (class=housing).
                ("node_id", ColType::Uuid),
                // Human-readable name / building name.
                ("name", ColType::String),
                ("address", ColType::String),
                ("city", ColType::StringNull),
                ("state_province", ColType::StringNull),
                ("postal_code", ColType::StringNull),
                // single_family | multi_family | condo | townhome | dormitory | shelter
                ("property_type", ColType::String),
                // Total declared unit slots.
                ("unit_count", ColType::Integer),
                // draft | pending_council | active | vacating | archived
                ("status", ColType::String),
                // Minimum council approvals required to activate (default 2).
                ("quorum_required", ColType::Integer),
                // Free-text notes from the submitting operator.
                ("notes", ColType::TextNull),
                // When the node was submitted for council review.
                ("submitted_at", ColType::TimestampWithTimeZoneNull),
                // When the node became active (quorum reached).
                ("activated_at", ColType::TimestampWithTimeZoneNull),
            ],
            &[],
        )
        .await?;

        // Index on node_id for fast reverse-lookup from the nodes table.
        m.create_index(
            IndexCreateStatement::new()
                .table(Alias::new("housing_nodes"))
                .col(Alias::new("node_id"))
                .name("idx_housing_nodes_node_id")
                .to_owned(),
        )
        .await?;

        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "housing_nodes").await?;
        Ok(())
    }
}
