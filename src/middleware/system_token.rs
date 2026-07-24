//! # System Token Guard
//!
//! Machine-to-machine authentication for trusted services (e.g. the Tea &
//! Madness publisher) that need to commission nodes and route payments through
//! the Nexus without a human JWT.
//!
//! A service presents its token in the `X-Nexus-System-Token` header. The guard
//! accepts either:
//! * the configured **root** token (env `NEXUS_SYSTEM_TOKEN`, with a dev
//!   fallback so the stack runs out of the box), which carries the `*` scope; or
//! * any active token in the `system_tokens` table, matched by SHA-256 hash,
//!   carrying that token's stored scopes.

use axum::http::HeaderMap;
use loco_rs::prelude::*;

use crate::models::system_tokens;

/// Header carrying the service token.
pub const HEADER: &str = "x-nexus-system-token";
/// Environment variable holding the root system token.
pub const ENV_ROOT_TOKEN: &str = "NEXUS_SYSTEM_TOKEN";
/// Development fallback root token. Override in any real deployment.
pub const DEV_ROOT_TOKEN: &str = "dev-nexus-system-token";

/// The validated identity of a calling service.
#[derive(Debug, Clone)]
pub struct SystemContext {
    pub service: String,
    pub scopes: Vec<String>,
}

impl SystemContext {
    /// Whether this service may perform `scope`. `"*"` grants all; a stored
    /// `prefix:*` grants any scope under that prefix.
    #[must_use]
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| {
            s == "*"
                || s == scope
                || (s.ends_with('*') && scope.starts_with(s.trim_end_matches('*')))
        })
    }
}

/// Constant-time string comparison to avoid leaking the root token via timing.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Resolve the configured root token, falling back to the dev value.
fn root_token() -> String {
    std::env::var(ENV_ROOT_TOKEN).unwrap_or_else(|_| DEV_ROOT_TOKEN.to_string())
}

/// Authenticate a request by its system token header, returning the calling
/// service's context. Does not check scopes — see [`require`].
pub async fn authenticate(ctx: &AppContext, headers: &HeaderMap) -> Result<SystemContext> {
    let token = headers
        .get(HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| loco_rs::Error::Unauthorized("missing system token".to_string()))?;

    if constant_time_eq(token, &root_token()) {
        return Ok(SystemContext {
            service: "root".to_string(),
            scopes: vec!["*".to_string()],
        });
    }

    let hash = system_tokens::hash_token(token);
    if let Some(record) = system_tokens::Model::find_active_by_hash(&ctx.db, &hash).await? {
        let scopes = record.scope_list();
        let service = record.service.clone();
        // Best-effort last-used bookkeeping; never block the request on it.
        let _ = record.touch(&ctx.db).await;
        return Ok(SystemContext { service, scopes });
    }

    Err(loco_rs::Error::Unauthorized(
        "invalid system token".to_string(),
    ))
}

/// Validate a raw token string, returning the calling service's context.
/// Shared by the header and bearer entry points.
pub async fn validate_token(ctx: &AppContext, token: &str) -> Result<SystemContext> {
    let token = token.trim();
    if token.is_empty() {
        return Err(loco_rs::Error::Unauthorized("empty token".to_string()));
    }

    if constant_time_eq(token, &root_token()) {
        return Ok(SystemContext {
            service: "root".to_string(),
            scopes: vec!["*".to_string()],
        });
    }

    let hash = system_tokens::hash_token(token);
    if let Some(record) = system_tokens::Model::find_active_by_hash(&ctx.db, &hash).await? {
        let scopes = record.scope_list();
        let service = record.service.clone();
        let _ = record.touch(&ctx.db).await;
        return Ok(SystemContext { service, scopes });
    }

    Err(loco_rs::Error::Unauthorized(
        "invalid system token".to_string(),
    ))
}

/// Authenticate a request by its `Authorization: Bearer <token>` header.
///
/// This is the entry point Daedalus IT and agents use — they present the Nexus
/// API key as a bearer token — as opposed to the `X-Nexus-System-Token` header
/// used by the Demiurge publisher integration. Both resolve to the same token
/// set (root token or a `system_tokens` row).
pub async fn authenticate_bearer(ctx: &AppContext, headers: &HeaderMap) -> Result<SystemContext> {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| loco_rs::Error::Unauthorized("missing bearer token".to_string()))?;

    validate_token(ctx, token).await
}

/// Authenticate and require a specific scope. Use this to guard service routes:
/// ```rust,ignore
/// let svc = system_token::require(&ctx, &headers, "nodes:create").await?;
/// ```
pub async fn require(ctx: &AppContext, headers: &HeaderMap, scope: &str) -> Result<SystemContext> {
    let svc = authenticate(ctx, headers).await?;
    if svc.has_scope(scope) {
        Ok(svc)
    } else {
        Err(loco_rs::Error::Unauthorized(format!(
            "system token for '{}' lacks scope '{}'",
            svc.service, scope
        )))
    }
}
