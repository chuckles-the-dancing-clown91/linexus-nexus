use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(
            m,
            "housing_occupancies",
            &[
                ("id", ColType::PkAuto),
                ("occupancy_id", ColType::Uuid),
                ("housing_unit_id", ColType::Integer),
                // `nodes`.id of the resident (class=human).
                ("resident_node_id", ColType::Integer),
                // `nodes`.id of the council member who assigned (nullable for system).
                ("assigned_by_node_id", ColType::IntegerNull),
                ("assigned_at", ColType::TimestampWithTimeZone),
                // NULL means the resident is currently occupying the unit.
                ("vacated_at", ColType::TimestampWithTimeZoneNull),
                // Free-form notes (special circumstances, transition notes).
                ("notes", ColType::TextNull),
            ],
            &[],
        )
        .await?;

        // Index to find current occupancy for a unit quickly.
        m.create_index(
            IndexCreateStatement::new()
                .table(Alias::new("housing_occupancies"))
                .col(Alias::new("housing_unit_id"))
                .name("idx_occupancies_unit")
                .to_owned(),
        )
        .await?;

        // Index to find all occupancies for a resident node.
        m.create_index(
            IndexCreateStatement::new()
                .table(Alias::new("housing_occupancies"))
                .col(Alias::new("resident_node_id"))
                .name("idx_occupancies_resident")
                .to_owned(),
        )
        .await?;

        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "housing_occupancies").await?;
        Ok(())
    }
}
