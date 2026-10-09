//! # Domains — Cloudflare Registrar (`docs/PROVIDERS.md` §6)
//!
//! Cloudflare's API cannot buy a domain; registration and transfers happen in
//! its dashboard. Once registered, a domain is read and managed here.

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use loco_rs::prelude::{get, AppContext, Routes};
use serde::Deserialize;
use serde_json::{json, Value};

use super::api::{operator, run_op, scope, ApiError, ApiJson, ApiResult, Op};
use crate::providers::{
    cloudflare::{domain_json, Cloudflare},
    dns::{normalize_zone_name, valid_zone_name, CF_PREFIX},
    ProviderResult, CLOUDFLARE,
};

/// Zone name → `cf:<id>`.
async fn zone_ids(cf: &Cloudflare) -> ProviderResult<HashMap<String, String>> {
    Ok(cf
        .zones()
        .await?
        .iter()
        .filter_map(|z| {
            let name = z.get("name").and_then(Value::as_str)?;
            let id = z.get("id").and_then(Value::as_str)?;
            Some((name.to_lowercase(), format!("{CF_PREFIX}{id}")))
        })
        .collect())
}

fn domain_name(raw: &str) -> ApiResult<String> {
    let name = normalize_zone_name(raw);
    if valid_zone_name(&name) {
        Ok(name)
    } else {
        Err(ApiError::not_found(format!("no such domain: {raw}")))
    }
}

async fn one(cf: &Cloudflare, name: &str) -> ApiResult<Value> {
    let d = cf.registrar_domain(name).await?;
    let zones = zone_ids(cf).await?;
    Ok(domain_json(&d, zones.get(name).map_or("", String::as_str)))
}

/// `GET /api/v1/domains`.
pub async fn list(State(ctx): State<AppContext>, headers: HeaderMap) -> ApiResult<Response> {
    operator(&ctx, &headers, scope::INFRA_READ).await?;
    let cf = Cloudflare::from_ctx(&ctx).await?;
    let domains = cf.registrar_domains().await?;
    let zones = zone_ids(&cf).await?;
    let out: Vec<Value> = domains
        .iter()
        .map(|d| {
            let projected = domain_json(d, "");
            let name = projected["name"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase();
            domain_json(d, zones.get(&name).map_or("", String::as_str))
        })
        .collect();
    Ok(axum::Json(out).into_response())
}

/// `GET /api/v1/domains/{name}`.
pub async fn show(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<Response> {
    operator(&ctx, &headers, scope::INFRA_READ).await?;
    let name = domain_name(&name)?;
    let cf = Cloudflare::from_ctx(&ctx).await?;
    Ok(axum::Json(one(&cf, &name).await?).into_response())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchDomain {
    #[serde(default)]
    pub auto_renew: Option<bool>,
    #[serde(default)]
    pub locked: Option<bool>,
    #[serde(default)]
    pub privacy: Option<bool>,
}

/// `PATCH /api/v1/domains/{name}` `{autoRenew?, locked?, privacy?}` → the domain.
pub async fn update(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(name): Path<String>,
    ApiJson(req): ApiJson<PatchDomain>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers, scope::INFRA_WRITE).await?;
    let name = domain_name(&name)?;
    let mut body = json!({});
    if let Some(v) = req.auto_renew {
        body["auto_renew"] = json!(v);
    }
    if let Some(v) = req.locked {
        body["locked"] = json!(v);
    }
    if let Some(v) = req.privacy {
        body["privacy"] = json!(v);
    }
    if body.as_object().is_some_and(serde_json::Map::is_empty) {
        return Err(ApiError::invalid(
            "one of autoRenew, locked, privacy is required",
        ));
    }
    let cf = Cloudflare::from_ctx(&ctx).await?;
    let op = Op::new(&headers, &svc, CLOUDFLARE, "domain.update", name.clone())?;
    run_op(&ctx, op, || async {
        cf.update_registrar_domain(&name, &body).await?;
        Ok((StatusCode::OK, one(&cf, &name).await?))
    })
    .await
}

pub fn routes() -> Routes {
    Routes::new()
        .prefix("api/v1")
        .add("/domains", get(list))
        .add("/domains/{name}", get(show).patch(update))
}
