use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Append-only mint ledger. Every mint is a fact appended here, never a balance
/// edited in place — state is always derived from the ledger, so the history is
/// the truth and the balance is only a view of it.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(
            m,
            "contributions",
            &[
                ("id", ColType::PkAuto),
                ("contribution_id", ColType::Uuid),
                ("node_id", ColType::Uuid),
                // labor | education | mentorship | care | emergency_response |
                // rehabilitation | content_creation
                ("kind", ColType::String),
                ("minutes", ColType::BigInteger),
                ("essential", ColType::Boolean),
                ("coverage", ColType::Boolean),
                ("week_index", ColType::BigInteger),
                ("minted_amount", ColType::BigInteger),
                ("at_unix", ColType::BigInteger),
                ("note", ColType::TextNull),
            ],
            &[],
        )
        .await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "contributions").await?;
        Ok(())
    }
}
