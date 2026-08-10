//! Give `agents` an environment and a tracking switch.
//!
//! Everything already on this table is a *fact the agent reported* — its
//! kernel, its core count, how long it has been up. These two columns are the
//! opposite: policy the Hub decides and the agent is told.
//!
//!   `environment`   — production / staging / development / whatever this
//!                     deployment calls its tiers. Defaults to `production`,
//!                     because an unclassified machine is safest read as one
//!                     that matters.
//!   `monitored`     — whether the machine counts at all. An agent that knows
//!                     it is unmonitored stops shipping telemetry, which is
//!                     the difference between a box that was powered down on
//!                     purpose and one that fell over.
//!   `monitor_note`  — why tracking is off, so the next person on call does
//!                     not have to go and ask.
//!
//! Nexus holds them because it is the inventory authority: an agent that is
//! reinstalled next week enrolls, reads its environment from here, and comes
//! back as the machine it was, rather than as a fresh production node that
//! immediately starts paging somebody.
//!
//! SQLite only supports one `ADD COLUMN` per `ALTER TABLE`, so each column is
//! added in its own statement (works on Postgres too).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[derive(DeriveIden)]
enum Agents {
    Table,
    Environment,
    Monitored,
    MonitorNote,
    EnvironmentUpdatedAt,
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        for def in [
            ColumnDef::new(Agents::Environment)
                .string_len(32)
                .not_null()
                .default("production")
                .to_owned(),
            ColumnDef::new(Agents::Monitored)
                .boolean()
                .not_null()
                .default(true)
                .to_owned(),
            ColumnDef::new(Agents::MonitorNote).text().null().to_owned(),
            ColumnDef::new(Agents::EnvironmentUpdatedAt)
                .timestamp_with_time_zone()
                .null()
                .to_owned(),
        ] {
            m.alter_table(
                Table::alter()
                    .table(Agents::Table)
                    .add_column(def)
                    .to_owned(),
            )
            .await?;
        }
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        for col in [
            Agents::Environment,
            Agents::Monitored,
            Agents::MonitorNote,
            Agents::EnvironmentUpdatedAt,
        ] {
            m.alter_table(
                Table::alter()
                    .table(Agents::Table)
                    .drop_column(col)
                    .to_owned(),
            )
            .await?;
        }
        Ok(())
    }
}
