use sea_orm_migration::prelude::*;

/// Adds the subscription `plan` tier to the users table.
///
/// Plans are a feature-gating axis (free / pro / enterprise) independent of
/// RBAC roles. Each plan maps to a permission bundle (see `models::plans`).
/// Defaults to `free` so existing rows and new sign-ups are valid without a
/// payment step.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.alter_table(
            Table::alter()
                .table(Alias::new("users"))
                .add_column(
                    ColumnDef::new(Alias::new("plan"))
                        .string()
                        .not_null()
                        .default("free"),
                )
                .to_owned(),
        )
        .await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.alter_table(
            Table::alter()
                .table(Alias::new("users"))
                .drop_column(Alias::new("plan"))
                .to_owned(),
        )
        .await?;
        Ok(())
    }
}
