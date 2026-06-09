use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Vicinagora payments routed through the Nexus. A payment may be settled in
/// fiat or in Demiurge. When settled in fiat, the *fees and taxes* are converted
/// to Demiurge and minted to the payee node — the friction the old world skims
/// is recaptured as contribution credit instead of leaking out of the node.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(
            m,
            "payments",
            &[
                ("id", ColType::PkAuto),
                ("payment_id", ColType::Uuid),
                // sale | donation | redemption | subscription
                ("kind", ColType::String),
                ("source", ColType::StringNull),
                ("payer_ref", ColType::StringNull),
                ("payee_node_id", ColType::UuidNull),
                // fiat | demiurge
                ("pay_currency", ColType::String),
                ("amount_fiat_minor", ColType::BigInteger),
                ("currency", ColType::String),
                ("fee_fiat_minor", ColType::BigInteger),
                ("tax_fiat_minor", ColType::BigInteger),
                ("demiurge_amount", ColType::BigInteger),
                ("demiurge_from_fees", ColType::BigInteger),
                // pending | processed | failed
                ("status", ColType::String),
                ("item_ref", ColType::StringNull),
                ("metadata", ColType::TextNull),
                ("processed_at", ColType::TimestampWithTimeZoneNull),
            ],
            &[],
        )
        .await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "payments").await?;
        Ok(())
    }
}
