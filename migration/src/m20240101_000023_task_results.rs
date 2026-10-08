//! Keep what the agent reported about a task, not just whether it finished.
//!
//! Until now a task row only remembered its lifecycle (`status`,
//! `completed_at`) and the first error. The result body the agent posts — its
//! own verdict, exit code, output and per-step outcomes — was forwarded once to
//! the Hub and then dropped, so nothing could ask Nexus afterwards what
//! happened. These columns hold it so `GET /api/v1/tasks/{id}` can serve it:
//!
//!   `result_status`  — the agent's verdict as sent (`success` / `failed`).
//!                      Null until a result arrives.
//!   `result_message` — the agent's one-line summary.
//!   `exit_code`      — the overall exit code (0 = every critical step
//!                      succeeded).
//!   `output`         — combined output, capped by the gateway (tail kept).
//!   `steps`          — per-step results, a JSON array.
//!
//! All nullable, so existing rows and older agents that send only
//! `{status, error, message}` stay valid.
//!
//! SQLite only supports one `ADD COLUMN` per `ALTER TABLE`, so each column is
//! added in its own statement (works on Postgres too).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[derive(DeriveIden)]
enum Tasks {
    Table,
    ResultStatus,
    ResultMessage,
    ExitCode,
    Output,
    Steps,
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        for def in [
            ColumnDef::new(Tasks::ResultStatus)
                .string_len(32)
                .null()
                .to_owned(),
            ColumnDef::new(Tasks::ResultMessage)
                .text()
                .null()
                .to_owned(),
            ColumnDef::new(Tasks::ExitCode)
                .big_integer()
                .null()
                .to_owned(),
            ColumnDef::new(Tasks::Output).text().null().to_owned(),
            ColumnDef::new(Tasks::Steps).text().null().to_owned(),
        ] {
            m.alter_table(
                Table::alter()
                    .table(Tasks::Table)
                    .add_column(def)
                    .to_owned(),
            )
            .await?;
        }
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        for col in [
            Tasks::ResultStatus,
            Tasks::ResultMessage,
            Tasks::ExitCode,
            Tasks::Output,
            Tasks::Steps,
        ] {
            m.alter_table(
                Table::alter()
                    .table(Tasks::Table)
                    .drop_column(col)
                    .to_owned(),
            )
            .await?;
        }
        Ok(())
    }
}
