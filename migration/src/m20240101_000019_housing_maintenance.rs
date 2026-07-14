use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(
            m,
            "housing_maintenance_tickets",
            &[
                ("id", ColType::PkAuto),
                ("ticket_id", ColType::Uuid),
                ("housing_unit_id", ColType::Integer),
                ("title", ColType::String),
                ("description", ColType::TextNull),
                // low | medium | high | emergency
                ("priority", ColType::String),
                // open | in_progress | resolved | closed
                ("status", ColType::String),
                // `nodes`.id of the worker assigned (nullable).
                ("assigned_to_node_id", ColType::IntegerNull),
                // `nodes`.id of the resident/council who opened the ticket.
                ("opened_by_node_id", ColType::IntegerNull),
                ("opened_at", ColType::TimestampWithTimeZone),
                ("resolved_at", ColType::TimestampWithTimeZoneNull),
                ("notes", ColType::TextNull),
            ],
            &[],
        )
        .await?;

        m.create_index(
            IndexCreateStatement::new()
                .table(Alias::new("housing_maintenance_tickets"))
                .col(Alias::new("housing_unit_id"))
                .name("idx_maintenance_unit")
                .to_owned(),
        )
        .await?;

        m.create_index(
            IndexCreateStatement::new()
                .table(Alias::new("housing_maintenance_tickets"))
                .col(Alias::new("status"))
                .name("idx_maintenance_status")
                .to_owned(),
        )
        .await?;

        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "housing_maintenance_tickets").await?;
        Ok(())
    }
}
