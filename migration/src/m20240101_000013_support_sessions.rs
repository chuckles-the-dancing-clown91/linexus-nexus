use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Remote-support session tracking. Each row is a complete lifecycle record for
/// one RustDesk remote-support session — from request through connection to
/// closure — providing the audit trail for compliance and security monitoring.
///
/// Converted from: src/components/remote_support/data/schemas/001_create_support_sessions.sql
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        // ── Table ────────────────────────────────────────────────────────
        create_table(
            m,
            "support_sessions",
            &[
                ("id", ColType::PkAuto),
                // UUIDv7 session identifier (time-ordered for better indexing)
                ("session_id", ColType::StringUniq),
                // Vault-encrypted connection credentials (peer_id:password:expiry)
                // Never stored in plaintext, decrypted only at connection time
                ("connection_token", ColType::String),
                // Relay server address for RustDesk connection
                ("relay_server", ColType::String),
                // Session lifecycle status:
                // 'pending', 'active', 'expired', 'closed', 'failed'
                ("status", ColType::String),
                // Timestamp when support was requested (ISO 8601)
                ("requested_at", ColType::TimestampWithTimeZone),
                // Token expiration timestamp (enforced by Vault policy TTL)
                ("expires_at", ColType::TimestampWithTimeZone),
                // Timestamp when RustDesk connection was established
                ("connected_at", ColType::TimestampWithTimeZoneNull),
                // Timestamp when session was terminated
                ("closed_at", ColType::TimestampWithTimeZoneNull),
                // Support engineer identity (extracted from mTLS certificate CN)
                ("engineer_id", ColType::StringNull),
                // User-provided reason for support request
                ("reason", ColType::TextNull),
            ],
            &[],
        )
        .await?;

        // ── Indexes ──────────────────────────────────────────────────────

        // Index for finding active sessions (hot path)
        m.create_index(
            Index::create()
                .name("idx_support_sessions_status")
                .table(Alias::new("support_sessions"))
                .col(Alias::new("status"))
                .to_owned(),
        )
        .await?;

        // Index for expiry cleanup job
        m.create_index(
            Index::create()
                .name("idx_support_sessions_expires_at")
                .table(Alias::new("support_sessions"))
                .col(Alias::new("expires_at"))
                .col(Alias::new("status"))
                .to_owned(),
        )
        .await?;

        // Index for audit log queries (by engineer)
        m.create_index(
            Index::create()
                .name("idx_support_sessions_engineer")
                .table(Alias::new("support_sessions"))
                .col(Alias::new("engineer_id"))
                .col(Alias::new("requested_at"))
                .to_owned(),
        )
        .await?;

        // Index for GDPR compliance cleanup (closed sessions older than 90 days)
        m.create_index(
            Index::create()
                .name("idx_support_sessions_closed_at")
                .table(Alias::new("support_sessions"))
                .col(Alias::new("closed_at"))
                .to_owned(),
        )
        .await?;

        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "support_sessions").await?;
        Ok(())
    }
}
