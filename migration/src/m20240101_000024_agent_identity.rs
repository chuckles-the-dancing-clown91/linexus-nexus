//! Give each agent its own identity and the richer facts it now reports.
//!
//!   `machine_id`          — `/etc/machine-id`; an agent that re-enrolls with
//!                           the same one keeps its agent id (re-adoption).
//!   `credential_hash`     — SHA-256 of the agent's own `nxa_` credential. The
//!                           plaintext is returned once, at enrollment.
//!   `enrollment_token_id` — the enrollment token it last enrolled with.
//!   `metadata`            — the Hub's ids carried on that token (JSON).
//!   `public_ip`, `interfaces`, `listening`, `services`, `packages`,
//!   `dns_server`          — reported facts (JSON text, portable across
//!                           SQLite and Postgres), stamped by `facts_at`.
//!   `dns_install_task_id` — the last `install_dns_server` task sent to it.
//!
//! `tasks.plan_request` keeps the body sent to the Orchestrator, so a task it
//! could not plan (`accepted`) can be re-planned later exactly as asked.
//!
//! SQLite only supports one `ADD COLUMN` per `ALTER TABLE`, so each column is
//! added in its own statement (works on Postgres too).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[derive(DeriveIden)]
enum Agents {
    Table,
    MachineId,
    CredentialHash,
    EnrollmentTokenId,
    Metadata,
    PublicIp,
    Interfaces,
    Listening,
    Services,
    Packages,
    DnsServer,
    FactsAt,
    DnsInstallTaskId,
}

#[derive(DeriveIden)]
enum Tasks {
    Table,
    PlanRequest,
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        for def in [
            ColumnDef::new(Agents::MachineId)
                .string_len(128)
                .null()
                .to_owned(),
            ColumnDef::new(Agents::CredentialHash)
                .string_len(64)
                .null()
                .to_owned(),
            ColumnDef::new(Agents::EnrollmentTokenId)
                .uuid()
                .null()
                .to_owned(),
            ColumnDef::new(Agents::Metadata).text().null().to_owned(),
            ColumnDef::new(Agents::PublicIp)
                .string_len(64)
                .null()
                .to_owned(),
            ColumnDef::new(Agents::Interfaces).text().null().to_owned(),
            ColumnDef::new(Agents::Listening).text().null().to_owned(),
            ColumnDef::new(Agents::Services).text().null().to_owned(),
            ColumnDef::new(Agents::Packages).text().null().to_owned(),
            ColumnDef::new(Agents::DnsServer).text().null().to_owned(),
            ColumnDef::new(Agents::FactsAt)
                .timestamp_with_time_zone()
                .null()
                .to_owned(),
            ColumnDef::new(Agents::DnsInstallTaskId)
                .string_len(64)
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
                .add_column(ColumnDef::new(Tasks::PlanRequest).text().null().to_owned())
                .to_owned(),
        )
        .await?;

        // Not unique: cloned images can share a machine id, and the lookup
        // has to keep working (it picks the oldest row) rather than refuse.
        m.create_index(
            Index::create()
                .name("idx_agents_machine_id")
                .table(Agents::Table)
                .col(Agents::MachineId)
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("idx_agents_credential_hash")
                .table(Agents::Table)
                .col(Agents::CredentialHash)
                .unique()
                .to_owned(),
        )
        .await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_index(
            Index::drop()
                .name("idx_agents_credential_hash")
                .table(Agents::Table)
                .to_owned(),
        )
        .await?;
        m.drop_index(
            Index::drop()
                .name("idx_agents_machine_id")
                .table(Agents::Table)
                .to_owned(),
        )
        .await?;
        m.alter_table(
            Table::alter()
                .table(Tasks::Table)
                .drop_column(Tasks::PlanRequest)
                .to_owned(),
        )
        .await?;
        for col in [
            Agents::MachineId,
            Agents::CredentialHash,
            Agents::EnrollmentTokenId,
            Agents::Metadata,
            Agents::PublicIp,
            Agents::Interfaces,
            Agents::Listening,
            Agents::Services,
            Agents::Packages,
            Agents::DnsServer,
            Agents::FactsAt,
            Agents::DnsInstallTaskId,
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
