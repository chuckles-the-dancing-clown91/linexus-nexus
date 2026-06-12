use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// A lot is a minted batch — an amount paired with the instant it was born.
/// Wallets are the set of a node's unexpired lots; balances are derived, never
/// stored. Age is non-transferable: a lot keeps its `minted_at_unix` no matter
/// how many hands it passes through, so decay cannot be laundered away.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(
            m,
            "demiurge_lots",
            &[
                ("id", ColType::PkAuto),
                ("node_id", ColType::Uuid),
                // Remaining amount in this lot (FIFO spend draws it down).
                ("amount", ColType::BigInteger),
                ("original_amount", ColType::BigInteger),
                ("minted_at_unix", ColType::BigInteger),
                ("expires_at_unix", ColType::BigInteger),
                // contribution | payment_fee | sponsorship | attestation | seed
                ("origin", ColType::String),
                ("origin_ref", ColType::StringNull),
            ],
            &[],
        )
        .await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "demiurge_lots").await?;
        Ok(())
    }
}
