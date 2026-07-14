use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(
            m,
            "housing_council_reviews",
            &[
                ("id", ColType::PkAuto),
                ("review_id", ColType::Uuid),
                ("housing_node_id", ColType::Integer),
                // The `nodes` pk (id) of the council member casting the vote.
                ("reviewer_node_id", ColType::Integer),
                // approve | reject
                ("vote", ColType::String),
                ("voted_at", ColType::TimestampWithTimeZone),
                // Optional reasoning for the vote record.
                ("notes", ColType::TextNull),
            ],
            &[],
        )
        .await?;

        // Unique: one vote per council member per housing node.
        m.create_index(
            IndexCreateStatement::new()
                .table(Alias::new("housing_council_reviews"))
                .col(Alias::new("housing_node_id"))
                .col(Alias::new("reviewer_node_id"))
                .name("idx_council_reviews_unique_vote")
                .unique()
                .to_owned(),
        )
        .await?;

        m.create_index(
            IndexCreateStatement::new()
                .table(Alias::new("housing_council_reviews"))
                .col(Alias::new("housing_node_id"))
                .name("idx_council_reviews_node")
                .to_owned(),
        )
        .await?;

        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "housing_council_reviews").await?;
        Ok(())
    }
}
