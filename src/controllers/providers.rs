//! # Providers and the operations log (`docs/PROVIDERS.md` §4, §9)
//!
//! Credentials are write-only: `PUT` seals a token with `NEXUS_SECRET_KEY`
//! (AES-256-GCM) and nothing ever reads it back out through the API.
//! Environment credentials win over stored ones.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use loco_rs::prelude::{get, post, put, AppContext, Routes};
use serde::Deserialize;
use serde_json::{json, Value};

use super::api::{operator, run_op, scope, ApiError, ApiJson, ApiResult, Op};
use crate::models::{provider_credentials, provider_operations};
use crate::providers::{
    self, cloudflare::Cloudflare, digitalocean::DigitalOcean, ProviderError, BIND, CLOUDFLARE,
    CREDENTIALED, DIGITALOCEAN,
};
use crate::secrets;

fn kind(key: &str) -> &'static str {
    if key == DIGITALOCEAN {
        "cloud"
    } else {
        "dns"
    }
}

fn credentialed_key(key: &str) -> ApiResult<&'static str> {
    match key {
        DIGITALOCEAN => Ok(DIGITALOCEAN),
        CLOUDFLARE => Ok(CLOUDFLARE),
        BIND => Err(ApiError::invalid(
            "bind takes no credentials: it is our agents",
        )),
        _ => Err(ApiError::not_found(format!("no such provider: {key}"))),
    }
}

fn time_or_null(t: Option<sea_orm::prelude::DateTimeWithTimeZone>) -> Value {
    t.map_or(Value::Null, |d| json!(d.to_rfc3339()))
}

/// One provider's entry in `GET /providers`.
async fn provider_entry(ctx: &AppContext, key: &'static str) -> ApiResult<Value> {
    let row = provider_credentials::Model::find_by_provider(&ctx.db, key).await?;
    let creds = providers::credentials(ctx, key).await;
    let (configured, source, mut detail) = match &creds {
        Ok(Some(c)) => (true, c.source.as_str(), String::new()),
        Ok(None) => (false, "none", String::new()),
        Err(e) => (false, "stored", e.to_string()),
    };
    let state = if configured {
        row.as_ref()
            .and_then(|r| r.state.clone())
            .unwrap_or_else(|| "unknown".to_string())
    } else {
        "not_configured".to_string()
    };
    if configured && detail.is_empty() {
        detail = row
            .as_ref()
            .and_then(|r| r.detail.clone())
            .unwrap_or_default();
    }
    let account_id = match &creds {
        Ok(Some(c)) => c
            .account_id
            .clone()
            .or_else(|| row.as_ref().and_then(|r| r.account_id.clone())),
        _ => None,
    };
    Ok(json!({
        "key": key,
        "kind": kind(key),
        "configured": configured,
        "source": source,
        "accountId": account_id.unwrap_or_default(),
        "accountName": if configured {
            row.as_ref().and_then(|r| r.account_name.clone()).unwrap_or_default()
        } else {
            String::new()
        },
        "checkedAt": if configured { time_or_null(row.as_ref().and_then(|r| r.checked_at)) } else { Value::Null },
        "state": state,
        "detail": detail,
    }))
}

/// `GET /api/v1/providers`.
pub async fn list(State(ctx): State<AppContext>, headers: HeaderMap) -> ApiResult<Response> {
    operator(&ctx, &headers, scope::INFRA_READ).await?;
    let mut out = Vec::new();
    for key in CREDENTIALED {
        out.push(provider_entry(&ctx, key).await?);
    }
    out.push(json!({
        "key": BIND,
        "kind": "dns",
        "configured": true,
        "source": "builtin",
        "accountId": "",
        "accountName": "",
        "checkedAt": Value::Null,
        "state": "ok",
        "detail": "served by Linexus agents",
    }));
    Ok(axum::Json(out).into_response())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsRequest {
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
}

/// `PUT /api/v1/providers/{key}/credentials` → `204`. With a `token`, the
/// token (and the account id as sent, blank = none) replace what is stored.
/// Without one, only the account id changes — blank clears it — and a token
/// must already be stored; so a wrong account id is fixed without pasting
/// the token again.
pub async fn put_credentials(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(key): Path<String>,
    ApiJson(req): ApiJson<CredentialsRequest>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers, scope::INFRA_WRITE).await?;
    let key = credentialed_key(&key)?;
    let token = req
        .token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(ToString::to_string);
    if let Some(t) = &token {
        if t.len() > 4096 || t.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(ApiError::invalid(
                "token: at most 4096 characters, no whitespace",
            ));
        }
    }
    let account_id = match req.account_id.as_deref().map(str::trim) {
        Some(a) if !a.is_empty() => {
            if a.len() > 64 || !a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                return Err(ApiError::invalid("accountId: letters, digits and dashes"));
            }
            Some(a.to_string())
        }
        _ => None,
    };
    let Some(token) = token else {
        // Only the account id. The token stays as it is — there has to be one.
        let stored = provider_credentials::Model::find_by_provider(&ctx.db, key)
            .await?
            .is_some_and(|r| r.sealed_token.is_some());
        if !stored {
            return Err(ApiError::invalid(
                "token: required (no token is stored for this provider yet)",
            ));
        }
        let op = Op::new(&headers, &svc, key, "credentials.account", key)?;
        return run_op(&ctx, op, || async {
            provider_credentials::Model::set_account(&ctx.db, key, account_id).await?;
            Ok((StatusCode::NO_CONTENT, Value::Null))
        })
        .await;
    };
    if !secrets::key_available(&ctx.environment) {
        return Err(ApiError::invalid(format!(
            "{} is not set; credentials cannot be stored",
            secrets::ENV_SECRET_KEY
        )));
    }
    let op = Op::new(&headers, &svc, key, "credentials.put", key)?;
    run_op(&ctx, op, || async {
        let sealed = secrets::seal(&ctx.environment, key, &token)
            .map_err(|e| ApiError::invalid(e.to_string()))?;
        provider_credentials::Model::store(&ctx.db, key, sealed, account_id).await?;
        Ok((StatusCode::NO_CONTENT, Value::Null))
    })
    .await
}

/// `DELETE /api/v1/providers/{key}/credentials` → `204`.
pub async fn delete_credentials(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers, scope::INFRA_WRITE).await?;
    let key = credentialed_key(&key)?;
    let op = Op::new(&headers, &svc, key, "credentials.delete", key)?;
    run_op(&ctx, op, || async {
        provider_credentials::Model::clear(&ctx.db, key).await?;
        Ok((StatusCode::NO_CONTENT, Value::Null))
    })
    .await
}

/// The test verdict for a provider failure.
fn failure_status(e: &ProviderError) -> provider_credentials::Status {
    let state = match e {
        ProviderError::Rejected {
            status: 401 | 403, ..
        }
        | ProviderError::NotConfigured(_) => "unauthorized",
        ProviderError::Unreachable(_) => "unreachable",
        _ => "unknown",
    };
    provider_credentials::Status {
        state: state.to_string(),
        detail: e.to_string(),
        ..Default::default()
    }
}

async fn test_digitalocean(do_: &DigitalOcean) -> (provider_credentials::Status, Option<Value>) {
    match do_.account().await {
        Ok(a) => {
            let team = a
                .pointer("/team/name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let name = if team.is_empty() {
                a.get("email").and_then(Value::as_str).unwrap_or_default()
            } else {
                team
            };
            let status = a.get("status").and_then(Value::as_str).unwrap_or("active");
            (
                provider_credentials::Status {
                    state: "ok".into(),
                    detail: if status == "active" {
                        String::new()
                    } else {
                        format!("account status: {status}")
                    },
                    account_id: a
                        .get("uuid")
                        .and_then(Value::as_str)
                        .map(ToString::to_string),
                    account_name: Some(name.to_string()),
                },
                None,
            )
        }
        Err(e) => (failure_status(&e), None),
    }
}

async fn test_cloudflare(
    cf: &Cloudflare,
    configured_account: Option<&str>,
) -> (provider_credentials::Status, Option<Value>) {
    // `/user/tokens/verify` only knows user-owned tokens. An account-owned token
    // (Manage Account → API Tokens) is verified at `/accounts/{id}/tokens/verify`,
    // and either kind that merely works is proved by reading a zone. A valid
    // token is never reported "unauthorized" for lacking one of those.
    let mut how = String::new();
    let verify = match cf.verify_token().await {
        Ok(v) => v,
        Err(
            e @ ProviderError::Rejected {
                status: 400 | 401 | 403,
                ..
            },
        ) => {
            let via_account = match configured_account {
                Some(id) => cf.verify_account_token(id).await.ok(),
                None => None,
            };
            if let Some(v) = via_account {
                v
            } else {
                match cf.probe().await {
                    Ok(zones) => {
                        how = format!(
                            "the token works (it reads {} zone{}); it is not a user token, so Cloudflare's verify call does not apply",
                            zones.len(),
                            if zones.len() == 1 { "" } else { "s" }
                        );
                        json!({"status": "active"})
                    }
                    // Neither worked: report the first, original refusal.
                    Err(_) => return (failure_status(&e), None),
                }
            }
        }
        Err(e) => return (failure_status(&e), None),
    };
    let token_status = verify
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let scopes = json!({
        "tokenStatus": token_status,
        "expiresOn": verify.get("expires_on").cloned().unwrap_or(Value::Null),
        "notBefore": verify.get("not_before").cloned().unwrap_or(Value::Null),
    });
    if token_status != "active" {
        return (
            provider_credentials::Status {
                state: "unauthorized".into(),
                detail: format!("token status: {token_status}"),
                ..Default::default()
            },
            Some(scopes),
        );
    }
    let accounts = match cf.accounts().await {
        Ok(a) => a,
        // A token without "Account Settings: Read" cannot list accounts. It is
        // still a working token; say what could not be checked.
        Err(ProviderError::Rejected {
            status: 401 | 403, ..
        }) => {
            let mut scopes = scopes;
            scopes["accounts"] = Value::Null;
            let mut detail =
                "the token works but cannot list accounts (no Account Settings: Read), so the account id could not be checked"
                    .to_string();
            if !how.is_empty() {
                detail = format!("{how}; {detail}");
            }
            return (
                provider_credentials::Status {
                    state: "ok".into(),
                    detail,
                    account_id: configured_account.map(ToString::to_string),
                    account_name: None,
                },
                Some(scopes),
            );
        }
        Err(e) => return (failure_status(&e), Some(scopes)),
    };
    let pick = |a: &Value| {
        (
            a.get("id").and_then(Value::as_str).map(ToString::to_string),
            a.get("name")
                .and_then(Value::as_str)
                .map(ToString::to_string),
        )
    };
    let mut scopes = scopes;
    scopes["accounts"] = json!(accounts.len());
    let status = match configured_account {
        Some(id) => match accounts
            .iter()
            .find(|a| a.get("id").and_then(Value::as_str) == Some(id))
        {
            Some(a) => {
                let (account_id, account_name) = pick(a);
                provider_credentials::Status {
                    state: "ok".into(),
                    detail: String::new(),
                    account_id,
                    account_name,
                }
            }
            None => provider_credentials::Status {
                state: "unauthorized".into(),
                detail: format!("the token cannot see account {id}"),
                account_id: Some(id.to_string()),
                account_name: None,
            },
        },
        None => match accounts.as_slice() {
            [one] => {
                let (account_id, account_name) = pick(one);
                provider_credentials::Status {
                    state: "ok".into(),
                    detail: String::new(),
                    account_id,
                    account_name,
                }
            }
            [] => provider_credentials::Status {
                state: "ok".into(),
                detail: "the token can see no account (Registrar and zone creation need one)"
                    .into(),
                ..Default::default()
            },
            _ => provider_credentials::Status {
                state: "ok".into(),
                detail: "the token can see several accounts; set accountId".into(),
                ..Default::default()
            },
        },
    };
    (status, Some(scopes))
}

/// `POST /api/v1/providers/{key}/test` → `{ok, state, accountId, accountName,
/// detail, scopes?}`. A provider without credentials is `424`; a provider
/// that refuses the token or cannot be reached answers `200` with `ok:
/// false` and the state.
pub async fn test(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> ApiResult<Response> {
    operator(&ctx, &headers, scope::INFRA_READ).await?;
    if key == BIND {
        return Ok(axum::Json(json!({
            "ok": true, "state": "ok", "accountId": "", "accountName": "",
            "detail": "served by Linexus agents",
        }))
        .into_response());
    }
    let key = credentialed_key(&key)?;
    let creds = providers::require_credentials(&ctx, key).await?;
    let (status, scopes) = if key == DIGITALOCEAN {
        test_digitalocean(&DigitalOcean::new(&creds)?).await
    } else {
        test_cloudflare(&Cloudflare::new(&creds)?, creds.account_id.as_deref()).await
    };
    provider_credentials::Model::record_status(
        &ctx.db,
        key,
        &status,
        creds.source == providers::Source::Stored,
    )
    .await?;
    let mut body = json!({
        "ok": status.state == "ok",
        "state": status.state,
        "accountId": status.account_id.clone().unwrap_or_default(),
        "accountName": status.account_name.clone().unwrap_or_default(),
        "detail": status.detail,
    });
    if let Some(s) = scopes {
        body["scopes"] = s;
    }
    Ok(axum::Json(body).into_response())
}

#[derive(Debug, Deserialize)]
pub struct OperationsQuery {
    pub provider: Option<String>,
    pub limit: Option<u64>,
}

/// `GET /api/v1/operations?provider=&limit=50` — newest first.
pub async fn operations(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Query(q): Query<OperationsQuery>,
) -> ApiResult<Response> {
    operator(&ctx, &headers, scope::INFRA_READ).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let provider = q
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty());
    let rows = provider_operations::Model::list(&ctx.db, provider, limit).await?;
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.operation_id.to_string(),
                "provider": r.provider,
                "operation": r.operation,
                "target": r.target.clone().unwrap_or_default(),
                "requester": r.requester.clone().unwrap_or_default(),
                "status": r.status,
                "error": r.error.clone().unwrap_or_default(),
                "idempotencyKey": r.idempotency_key.clone().unwrap_or_default(),
                "createdAt": r.created_at.to_rfc3339(),
            })
        })
        .collect();
    Ok(axum::Json(out).into_response())
}

pub fn routes() -> Routes {
    Routes::new()
        .prefix("api/v1")
        .add("/providers", get(list))
        .add(
            "/providers/{key}/credentials",
            put(put_credentials).delete(delete_credentials),
        )
        .add("/providers/{key}/test", post(test))
        .add("/operations", get(operations))
}
