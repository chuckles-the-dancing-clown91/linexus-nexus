use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(
            m,
            "housing_queue",
            &[
                ("id", ColType::PkAuto),
                ("queue_id", ColType::Uuid),
                ("housing_unit_id", ColType::Integer),
                // Snapshot of unit address+number for quick display without join.
                ("unit_label", ColType::String),
                // available | reserved | assigned
                ("status", ColType::String),
                // When this entry entered the queue (FIFO ordering key).
                ("queued_at", ColType::TimestampWithTimeZone),
                // When the unit was claimed / assigned (NULL while available).
                ("claimed_at", ColType::TimestampWithTimeZoneNull),
                // `nodes`.id of the resident who was assigned (set on claim).
                ("claimed_by_node_id", ColType::IntegerNull),
                // Priority override: 0=normal, higher values bubble up.
                ("priority", ColType::Integer),
            ],
            &[],
        )
        .await?;

        m.create_index(
            IndexCreateStatement::new()
                .table(Alias::new("housing_queues"))
                .col(Alias::new("housing_unit_id"))
                .name("idx_housing_queue_unit")
                .to_owned(),
        )
        .await?;

        m.create_index(
            IndexCreateStatement::new()
                .table(Alias::new("housing_queues"))
                .col(Alias::new("status"))
                .col(Alias::new("priority"))
                .col(Alias::new("queued_at"))
                .name("idx_housing_queue_status_order")
                .to_owned(),
        )
        .await?;

        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "housing_queue").await?;
        Ok(())
    }
}
