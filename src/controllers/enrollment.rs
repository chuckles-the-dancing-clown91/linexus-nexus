//! # Enrollment tokens (`docs/PROVIDERS.md` §1)
//!
//! The Hub mints a token bound to a hostgroup (the client's slug), an
//! environment and its own ids; the install command carries it; the agent
//! enrolls with it (`POST /api/v1/agents/enroll`) and the Hub reads the token
//! back to learn the new agent id.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use loco_rs::prelude::{get, AppContext, Routes};
use serde::Deserialize;
use serde_json::{json, Value};

use super::api::{operator, ApiError, ApiJson, ApiResult};
use crate::models::enrollment_tokens::{self, MintParams};

pub const DEFAULT_TTL_MINUTES: i64 = 1440;
pub const MIN_TTL_MINUTES: i64 = 5;
pub const MAX_TTL_MINUTES: i64 = 43_200;
pub const MAX_USES: i64 = 1000;
const MAX_METADATA_BYTES: usize = 8 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MintRequest {
    #[serde(default)]
    pub hostgroup: Option<String>,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub ttl_minutes: Option<i64>,
    #[serde(default)]
    pub max_uses: Option<i64>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

/// A hostgroup: 1–128 of `[A-Za-z0-9._:-]`.
#[must_use]
pub fn valid_hostgroup(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 128
        && h.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

/// An environment: 1–32 of `[a-z0-9_-]` (lowercased).
fn normalize_environment(e: Option<&str>) -> ApiResult<String> {
    let e = e
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| crate::models::agents::DEFAULT_ENVIRONMENT.to_string());
    if e.len() > 32
        || !e
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(ApiError::invalid("environment: 1–32 of a-z, 0-9, _ and -"));
    }
    Ok(e)
}

/// Validate a mint request into model params.
pub fn mint_params(req: &MintRequest, created_by: Option<String>) -> ApiResult<MintParams> {
    let hostgroup = req
        .hostgroup
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .ok_or_else(|| ApiError::invalid("hostgroup: required"))?;
    if !valid_hostgroup(hostgroup) {
        return Err(ApiError::invalid(
            "hostgroup: 1–128 of A-Z, a-z, 0-9, ., _, : and -",
        ));
    }
    let environment = normalize_environment(req.environment.as_deref())?;
    let label = match req.label.as_deref().map(str::trim) {
        Some(l) if !l.is_empty() => {
            if l.chars().count() > 200 || l.chars().any(char::is_control) {
                return Err(ApiError::invalid("label: at most 200 printable characters"));
            }
            Some(l.to_string())
        }
        _ => None,
    };
    let ttl = req.ttl_minutes.unwrap_or(DEFAULT_TTL_MINUTES);
    if !(MIN_TTL_MINUTES..=MAX_TTL_MINUTES).contains(&ttl) {
        return Err(ApiError::invalid("ttlMinutes: 5…43200"));
    }
    let max_uses = req.max_uses.unwrap_or(1);
    if !(1..=MAX_USES).contains(&max_uses) {
        return Err(ApiError::invalid("maxUses: 1…1000"));
    }
    let metadata = match &req.metadata {
        None | Some(Value::Null) => None,
        Some(m @ Value::Object(_)) => {
            if m.to_string().len() > MAX_METADATA_BYTES {
                return Err(ApiError::invalid("metadata: at most 8 KiB"));
            }
            Some(m.clone())
        }
        Some(_) => return Err(ApiError::invalid("metadata: must be an object")),
    };
    Ok(MintParams {
        hostgroup: hostgroup.to_string(),
        environment,
        label,
        metadata,
        ttl_minutes: ttl,
        max_uses: i32::try_from(max_uses).unwrap_or(1),
        created_by,
    })
}

/// The token shape. `token` only on creation.
#[must_use]
pub fn token_json(t: &enrollment_tokens::Model, plaintext: Option<&str>) -> Value {
    let mut v = json!({
        "id": t.token_id.to_string(),
        "hostgroup": t.hostgroup,
        "environment": t.environment,
        "label": t.label.clone().unwrap_or_default(),
        "metadata": t.metadata_value(),
        "expiresAt": t.expires_at.to_rfc3339(),
        "maxUses": t.max_uses,
        "uses": t.uses,
        "agents": t.agent_list(),
        "revokedAt": t.revoked_at.map_or(Value::Null, |d| json!(d.to_rfc3339())),
        "createdAt": t.created_at.to_rfc3339(),
    });
    if let Some(p) = plaintext {
        v["token"] = json!(p);
    }
    v
}

async fn find(ctx: &AppContext, id: &str) -> ApiResult<enrollment_tokens::Model> {
    let uuid =
        uuid::Uuid::parse_str(id).map_err(|_| ApiError::not_found("no such enrollment token"))?;
    enrollment_tokens::Model::find_by_token_id(&ctx.db, &uuid)
        .await
        .map_err(|e| match e {
            loco_rs::model::ModelError::EntityNotFound => {
                ApiError::not_found("no such enrollment token")
            }
            other => other.into(),
        })
}

/// `POST /api/v1/enrollment-tokens` → `201` with the one-time `token`.
pub async fn create(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    ApiJson(req): ApiJson<MintRequest>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers).await?;
    let created_by = super::api::requester(&headers, &svc)?;
    let params = mint_params(&req, Some(created_by))?;
    let (row, plaintext) = enrollment_tokens::Model::mint(&ctx.db, &params).await?;
    tracing::info!(token_id = %row.token_id, hostgroup = %row.hostgroup, "enrollment token minted");
    Ok((
        StatusCode::CREATED,
        axum::Json(token_json(&row, Some(&plaintext))),
    )
        .into_response())
}

/// `GET /api/v1/enrollment-tokens` — newest first, without `token`.
pub async fn list(State(ctx): State<AppContext>, headers: HeaderMap) -> ApiResult<Response> {
    operator(&ctx, &headers).await?;
    let rows = enrollment_tokens::Model::list(&ctx.db).await?;
    let out: Vec<Value> = rows.iter().map(|t| token_json(t, None)).collect();
    Ok(axum::Json(out).into_response())
}

/// `GET /api/v1/enrollment-tokens/{id}`.
pub async fn show(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    operator(&ctx, &headers).await?;
    Ok(axum::Json(token_json(&find(&ctx, &id).await?, None)).into_response())
}

/// `DELETE /api/v1/enrollment-tokens/{id}` — revoke → `204`.
pub async fn revoke(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    operator(&ctx, &headers).await?;
    let row = find(&ctx, &id).await?.revoke(&ctx.db).await?;
    tracing::info!(token_id = %row.token_id, "enrollment token revoked");
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub fn routes() -> Routes {
    Routes::new()
        .prefix("api/v1")
        .add("/enrollment-tokens", get(list).post(create))
        .add("/enrollment-tokens/{id}", get(show).delete(revoke))
}
