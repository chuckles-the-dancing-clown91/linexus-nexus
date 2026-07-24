//! Extend `agents` with the hardware/identity facts Daedalus IT surfaces on a
//! machine profile, and give `tasks` a column to hold the orchestrator's plan.
//!
//! SQLite only supports one `ADD COLUMN` per `ALTER TABLE`, so each column is
//! added in its own statement (works on Postgres too).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[derive(DeriveIden)]
enum Agents {
    Table,
    Hostgroup,
    Os,
    Kernel,
    Arch,
    CpuCores,
    MemoryMb,
    DiskGb,
    AgentVersion,
    UptimeSeconds,
}

#[derive(DeriveIden)]
enum Tasks {
    Table,
    Plan,
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        for def in [
            ColumnDef::new(Agents::Hostgroup).text().null().to_owned(),
            ColumnDef::new(Agents::Os).text().null().to_owned(),
            ColumnDef::new(Agents::Kernel).text().null().to_owned(),
            ColumnDef::new(Agents::Arch).text().null().to_owned(),
            ColumnDef::new(Agents::CpuCores).integer().null().to_owned(),
            ColumnDef::new(Agents::MemoryMb).integer().null().to_owned(),
            ColumnDef::new(Agents::DiskGb).integer().null().to_owned(),
            ColumnDef::new(Agents::AgentVersion)
                .text()
                .null()
                .to_owned(),
            ColumnDef::new(Agents::UptimeSeconds)
                .big_integer()
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

        m.alter_table(
            Table::alter()
                .table(Tasks::Table)
                .add_column(ColumnDef::new(Tasks::Plan).text().null())
                .to_owned(),
        )
        .await?;

        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        for col in [
            Agents::Hostgroup,
            Agents::Os,
            Agents::Kernel,
            Agents::Arch,
            Agents::CpuCores,
            Agents::MemoryMb,
            Agents::DiskGb,
            Agents::AgentVersion,
            Agents::UptimeSeconds,
        ] {
            m.alter_table(
                Table::alter()
                    .table(Agents::Table)
                    .drop_column(col)
                    .to_owned(),
            )
            .await?;
        }

        m.alter_table(
            Table::alter()
                .table(Tasks::Table)
                .drop_column(Tasks::Plan)
                .to_owned(),
        )
        .await?;

        Ok(())
    }
}
