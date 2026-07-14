use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(
            m,
            "housing_documents",
            &[
                ("id", ColType::PkAuto),
                ("document_id", ColType::Uuid),
                ("housing_node_id", ColType::Integer),
                // Optional association to a specific unit.
                ("housing_unit_id", ColType::IntegerNull),
                // council_resolution | inspection_report | occupancy_agreement |
                // maintenance_report | other
                ("document_kind", ColType::String),
                ("title", ColType::String),
                // Object-store URL or CDN link. Inline BLOB not used here.
                ("url", ColType::StringNull),
                // Inline content for small text documents (HTML / plaintext).
                ("content", ColType::TextNull),
                // `nodes`.id of who uploaded.
                ("uploaded_by_node_id", ColType::IntegerNull),
                ("uploaded_at", ColType::TimestampWithTimeZone),
            ],
            &[],
        )
        .await?;

        m.create_index(
            IndexCreateStatement::new()
                .table(Alias::new("housing_documents"))
                .col(Alias::new("housing_node_id"))
                .name("idx_housing_docs_node")
                .to_owned(),
        )
        .await?;

        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "housing_documents").await?;
        Ok(())
    }
}
