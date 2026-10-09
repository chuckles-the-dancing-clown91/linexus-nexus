//! Shared plumbing for the `/api/v1` provider surface (`docs/PROVIDERS.md`):
//! the `{"error": "<code>", "detail": "…"}` error shape, a JSON body extractor
//! that answers `400 invalid` naming the bad field, operator authentication,
//! the `X-Requested-By` / `Idempotency-Key` / `X-Confirm` headers, and the
//! operations log every provider mutation goes through ([`run_op`]).

use std::collections::HashSet;
use std::future::Future;
use std::sync::{Mutex, OnceLock};

use axum::{
    body::Bytes,
    extract::{FromRequest, Request},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use loco_rs::{app::AppContext, model::ModelError};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::gateway_client;
use crate::middleware::system_token::{self, SystemContext};
use crate::models::provider_operations::{self, NewOperation};
use crate::providers::ProviderError;

/// An error answered as `{"error": code, "detail": detail}`.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub detail: String,
}

pub type ApiResult<T> = std::result::Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            code,
            detail: detail.into(),
        }
    }
    pub fn invalid(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid", detail)
    }
    pub fn unauthorized(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", detail)
    }
    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", detail)
    }
    pub fn conflict(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", detail)
    }
    pub fn confirmation_required(detail: impl Into<String>) -> Self {
        Self::new(
            StatusCode::PRECONDITION_FAILED,
            "confirmation_required",
            detail,
        )
    }
    pub fn internal(detail: &impl std::fmt::Display) -> Self {
        // The cause goes to the log, never to the caller.
        tracing::error!(error = %detail, "internal error on the provider surface");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "internal error; see the Nexus log",
        )
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.status.as_u16(), self.code, self.detail)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            axum::Json(json!({ "error": self.code, "detail": self.detail })),
        )
            .into_response()
    }
}

impl From<ProviderError> for ApiError {
    fn from(e: ProviderError) -> Self {
        match e {
            ProviderError::NotConfigured(d) => {
                Self::new(StatusCode::FAILED_DEPENDENCY, "provider_not_configured", d)
            }
            ProviderError::Rejected { status, message } => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "provider_rejected",
                format!("provider answered {status}: {message}"),
            ),
            ProviderError::NotFound(d) => Self::not_found(d),
            ProviderError::Conflict(d) => Self::conflict(d),
            ProviderError::Unreachable(d) => {
                Self::new(StatusCode::BAD_GATEWAY, "provider_unreachable", d)
            }
            ProviderError::Invalid(d) => Self::invalid(d),
        }
    }
}

impl From<ModelError> for ApiError {
    fn from(e: ModelError) -> Self {
        match e {
            ModelError::EntityNotFound => Self::not_found("no such entity"),
            ModelError::EntityAlreadyExists => Self::conflict("already exists"),
            other => Self::internal(&other),
        }
    }
}

impl From<sea_orm::DbErr> for ApiError {
    fn from(e: sea_orm::DbErr) -> Self {
        Self::internal(&e)
    }
}

impl From<loco_rs::Error> for ApiError {
    fn from(e: loco_rs::Error) -> Self {
        match e {
            loco_rs::Error::Unauthorized(d) => Self::unauthorized(d),
            loco_rs::Error::BadRequest(d) => Self::invalid(d),
            loco_rs::Error::NotFound => Self::not_found("not found"),
            loco_rs::Error::Model(m) => m.into(),
            other => Self::internal(&other),
        }
    }
}

/// A JSON body whose rejection is `400 invalid` with serde's message (which
/// names the missing or malformed field). An empty body reads as `{}`.
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = Bytes::from_request(req, state)
            .await
            .map_err(|e| ApiError::invalid(format!("body: {e}")))?;
        let slice: &[u8] = if bytes.iter().all(u8::is_ascii_whitespace) {
            b"{}"
        } else {
            &bytes
        };
        serde_json::from_slice(slice)
            .map(ApiJson)
            .map_err(|e| ApiError::invalid(format!("body: {e}")))
    }
}

/// Authenticate an operator (system key). Agent credentials are refused.
pub async fn operator(ctx: &AppContext, headers: &HeaderMap) -> ApiResult<SystemContext> {
    Ok(system_token::authenticate_bearer(ctx, headers).await?)
}

fn header_text(headers: &HeaderMap, name: &str, max: usize) -> ApiResult<Option<String>> {
    let Some(v) = headers.get(name) else {
        return Ok(None);
    };
    let s = v
        .to_str()
        .map_err(|_| ApiError::invalid(format!("{name}: not printable ASCII")))?
        .trim();
    if s.is_empty() {
        return Ok(None);
    }
    if s.len() > max || s.chars().any(char::is_control) {
        return Err(ApiError::invalid(format!(
            "{name}: at most {max} printable characters"
        )));
    }
    Ok(Some(s.to_string()))
}

/// Who asked for a mutation: `X-Requested-By` (the Hub sends `user:<uuid>
/// <email>`), else the calling service.
pub fn requester(headers: &HeaderMap, svc: &SystemContext) -> ApiResult<String> {
    Ok(header_text(headers, "x-requested-by", 256)?
        .unwrap_or_else(|| format!("service:{}", svc.service)))
}

/// The `Idempotency-Key` header, if any.
pub fn idempotency_key(headers: &HeaderMap) -> ApiResult<Option<String>> {
    header_text(headers, "idempotency-key", 255)
}

/// Require `X-Confirm: <expected>` for a destructive call.
pub fn require_confirm(headers: &HeaderMap, expected: &str) -> ApiResult<()> {
    let got = headers
        .get("x-confirm")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .unwrap_or_default();
    if !expected.is_empty() && got == expected {
        Ok(())
    } else {
        Err(ApiError::confirmation_required(format!(
            "send X-Confirm: {expected} to confirm"
        )))
    }
}

/// A JSON answer with a status.
pub fn respond(status: StatusCode, body: &Value) -> Response {
    if status == StatusCode::NO_CONTENT {
        return status.into_response();
    }
    (status, axum::Json(body.clone())).into_response()
}

/// Who and how, for one provider mutation.
#[derive(Debug, Clone)]
pub struct Op {
    pub provider: &'static str,
    pub operation: &'static str,
    pub target: String,
    pub requester: String,
    pub idempotency_key: Option<String>,
}

impl Op {
    pub fn new(
        headers: &HeaderMap,
        svc: &SystemContext,
        provider: &'static str,
        operation: &'static str,
        target: impl Into<String>,
    ) -> ApiResult<Self> {
        Ok(Self {
            provider,
            operation,
            target: target.into(),
            requester: requester(headers, svc)?,
            idempotency_key: idempotency_key(headers)?,
        })
    }
}

fn in_flight() -> &'static Mutex<HashSet<String>> {
    static KEYS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    KEYS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Holds an idempotency key as in flight until dropped.
struct InFlight(String);

impl InFlight {
    fn claim(key: &str) -> Option<Self> {
        let mut set = in_flight()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        set.insert(key.to_string()).then(|| Self(key.to_string()))
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        in_flight()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.0);
    }
}

/// Run one provider mutation through the operations log.
///
/// * With an `Idempotency-Key` that already succeeded (same provider and
///   operation) within 24 h, the stored answer is replayed (with
///   `Idempotent-Replayed: true`) and `f` is not run. The same key for a
///   different operation, or while the first request is still running, is
///   `409 conflict`.
/// * Otherwise `f` runs; its outcome becomes a `provider_operations` row and a
///   Logger line (`source: "nexus.providers"`). Only successes replay; a
///   failure can be retried with the same key.
pub async fn run_op<F, Fut>(ctx: &AppContext, op: Op, f: F) -> ApiResult<Response>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ApiResult<(StatusCode, Value)>>,
{
    let _guard = match &op.idempotency_key {
        Some(key) => {
            if let Some(prev) = provider_operations::Model::find_replay(&ctx.db, key).await? {
                if prev.provider != op.provider || prev.operation != op.operation {
                    return Err(ApiError::conflict(format!(
                        "Idempotency-Key was already used for {} {}",
                        prev.provider, prev.operation
                    )));
                }
                let status = prev
                    .response_status
                    .and_then(|s| u16::try_from(s).ok())
                    .and_then(|s| StatusCode::from_u16(s).ok())
                    .unwrap_or(StatusCode::OK);
                let body = prev
                    .response
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or(Value::Null);
                let mut resp = respond(status, &body);
                resp.headers_mut()
                    .insert("idempotent-replayed", HeaderValue::from_static("true"));
                return Ok(resp);
            }
            Some(InFlight::claim(key).ok_or_else(|| {
                ApiError::conflict("a request with this Idempotency-Key is still in progress")
            })?)
        }
        None => None,
    };

    let outcome = f().await;
    let (ok, error, status, response) = match &outcome {
        Ok((status, body)) => (true, None, Some(status.as_u16()), Some(body.to_string())),
        Err(e) => (
            false,
            Some(format!("{} {}", e.code, e.detail)),
            Some(e.status.as_u16()),
            None,
        ),
    };
    let row = provider_operations::Model::record(
        &ctx.db,
        NewOperation {
            provider: op.provider.to_string(),
            operation: op.operation.to_string(),
            target: op.target.clone(),
            requester: Some(op.requester.clone()),
            ok,
            error: error.clone(),
            idempotency_key: op.idempotency_key.clone(),
            response_status: status,
            response,
        },
    )
    .await;
    let operation_id = match row {
        Ok(r) => r.operation_id.to_string(),
        Err(e) => {
            tracing::error!(error = %e, "failed to record provider operation");
            String::new()
        }
    };

    let line = json!({
        "level": if ok { "info" } else { "error" },
        "source": "nexus.providers",
        "message": format!(
            "{} {} {}: {}",
            op.provider,
            op.operation,
            op.target,
            if ok { "ok".to_string() } else { error.clone().unwrap_or_default() }
        ),
        "metadata": {
            "operationId": operation_id,
            "provider": op.provider,
            "operation": op.operation,
            "target": op.target,
            "requester": op.requester,
            "status": if ok { "ok" } else { "failed" },
            "error": error,
            "idempotencyKey": op.idempotency_key,
        },
    });
    tokio::spawn(async move {
        if let Err(e) = gateway_client::ship_log(&line).await {
            tracing::debug!(error = %e, "failed to ship provider operation log");
        }
    });

    outcome.map(|(status, body)| respond(status, &body))
}
