use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(
            m,
            "housing_units",
            &[
                ("id", ColType::PkAuto),
                ("housing_node_id", ColType::Integer),
                // e.g. "1A", "Unit 3", "Main Floor"
                ("unit_number", ColType::String),
                ("beds", ColType::IntegerNull),
                ("baths", ColType::FloatNull),
                ("sqft", ColType::IntegerNull),
                // available | occupied | make_ready | maintenance | down
                ("status", ColType::String),
                ("notes", ColType::TextNull),
            ],
            &[],
        )
        .await?;

        m.create_index(
            IndexCreateStatement::new()
                .table(Alias::new("housing_units"))
                .col(Alias::new("housing_node_id"))
                .name("idx_housing_units_node")
                .to_owned(),
        )
        .await?;

        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "housing_units").await?;
        Ok(())
    }
}
