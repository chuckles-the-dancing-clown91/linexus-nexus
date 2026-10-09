//! # DNS — one API across Cloudflare and BIND (`docs/PROVIDERS.md` §5)

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use loco_rs::prelude::{get, patch, post, AppContext, Routes};
use serde::Deserialize;
use serde_json::{json, Value};

use super::api::{
    operator, requester, require_confirm, run_op, scope, ApiError, ApiJson, ApiResult, Op,
};
use crate::dispatch::{self, DispatchRequest};
use crate::models::agents;
use crate::providers::{
    self,
    dns::{self, normalize_name, CreateZone, RecordChange, RecordInput, BIND_PREFIX},
    BIND, CLOUDFLARE,
};

pub const INTENT_INSTALL_DNS_SERVER: &str = "install_dns_server";

fn provider_key(zone_id: &str) -> &'static str {
    if zone_id.starts_with(BIND_PREFIX) {
        BIND
    } else {
        CLOUDFLARE
    }
}

fn zone_value(z: &dns::Zone) -> Value {
    serde_json::to_value(z).unwrap_or(Value::Null)
}

fn change_value(c: &RecordChange) -> Value {
    let mut v = json!({ "record": c.record });
    if let Some(t) = &c.task_id {
        v["taskId"] = json!(t);
    }
    v
}

/// `GET /api/v1/dns/zones` — every zone of every configured provider.
pub async fn list_zones(State(ctx): State<AppContext>, headers: HeaderMap) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers, scope::INFRA_READ).await?;
    let who = requester(&headers, &svc)?;
    let mut zones = Vec::new();
    if providers::credentials(&ctx, CLOUDFLARE).await?.is_some() {
        zones.extend(
            dns::provider_named(&ctx, CLOUDFLARE, &who)
                .await?
                .list_zones()
                .await?,
        );
    }
    zones.extend(
        dns::provider_named(&ctx, BIND, &who)
            .await?
            .list_zones()
            .await?,
    );
    let out: Vec<Value> = zones.iter().map(zone_value).collect();
    Ok(axum::Json(out).into_response())
}

/// `POST /api/v1/dns/zones` → `201` zone.
pub async fn create_zone(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    ApiJson(req): ApiJson<CreateZone>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers, scope::INFRA_WRITE).await?;
    let provider: &'static str = match req.provider.trim() {
        CLOUDFLARE => CLOUDFLARE,
        BIND => BIND,
        _ => return Err(ApiError::invalid("provider: must be cloudflare or bind")),
    };
    let name = dns::normalize_zone_name(&req.name);
    if !dns::valid_zone_name(&name) {
        return Err(ApiError::invalid("name: not a valid zone name"));
    }
    let op = Op::new(&headers, &svc, provider, "zone.create", name)?;
    let who = op.requester.clone();
    run_op(&ctx, op, || async {
        let p = dns::provider_named(&ctx, provider, &who).await?;
        let zone = p.create_zone(&req).await?;
        Ok((StatusCode::CREATED, zone_value(&zone)))
    })
    .await
}

/// `GET /api/v1/dns/zones/{zoneId}` — the zone with `records`.
pub async fn get_zone(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(zone_id): Path<String>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers, scope::INFRA_READ).await?;
    let (p, id) = dns::provider_for(&ctx, &zone_id, &requester(&headers, &svc)?).await?;
    Ok(axum::Json(zone_value(&p.get_zone(&id, true).await?)).into_response())
}

/// `DELETE /api/v1/dns/zones/{zoneId}` — needs `X-Confirm: <zone name>`.
pub async fn delete_zone(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(zone_id): Path<String>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers, scope::INFRA_WRITE).await?;
    let who = requester(&headers, &svc)?;
    let (p, id) = dns::provider_for(&ctx, &zone_id, &who).await?;
    let zone = p.get_zone(&id, false).await?;
    require_confirm(&headers, &zone.name)?;
    let op = Op::new(
        &headers,
        &svc,
        provider_key(&zone_id),
        "zone.delete",
        zone.name.clone(),
    )?;
    run_op(&ctx, op, || async {
        p.delete_zone(&id).await?;
        Ok((StatusCode::NO_CONTENT, Value::Null))
    })
    .await
}

#[derive(Debug, Deserialize)]
pub struct RecordsQuery {
    #[serde(rename = "type")]
    pub record_type: Option<String>,
    pub name: Option<String>,
}

/// `GET /api/v1/dns/zones/{zoneId}/records?type=&name=`.
pub async fn list_records(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(zone_id): Path<String>,
    Query(q): Query<RecordsQuery>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers, scope::INFRA_READ).await?;
    let (p, id) = dns::provider_for(&ctx, &zone_id, &requester(&headers, &svc)?).await?;
    let record_type = q
        .record_type
        .as_deref()
        .map(|t| t.trim().to_uppercase())
        .filter(|t| !t.is_empty());
    if let Some(t) = &record_type {
        if !dns::TYPES.contains(&t.as_str()) {
            return Err(ApiError::invalid(format!(
                "type: must be one of {}",
                dns::TYPES.join(", ")
            )));
        }
    }
    let name = match q.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => {
            let zone = p.get_zone(&id, false).await?;
            Some(normalize_name(n, &zone.name)?)
        }
        None => None,
    };
    let records = p
        .list_records(&id, record_type.as_deref(), name.as_deref())
        .await?;
    Ok(axum::Json(records).into_response())
}

fn record_target(zone_id: &str, input: &RecordInput) -> String {
    format!(
        "{zone_id} {} {}",
        input.record_type.clone().unwrap_or_default().to_uppercase(),
        input.name.clone().unwrap_or_default()
    )
}

/// `POST /api/v1/dns/zones/{zoneId}/records` → `201 {record, taskId?}`.
pub async fn create_record(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(zone_id): Path<String>,
    ApiJson(input): ApiJson<RecordInput>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers, scope::INFRA_WRITE).await?;
    let op = Op::new(
        &headers,
        &svc,
        provider_key(&zone_id),
        "record.create",
        record_target(&zone_id, &input),
    )?;
    let (p, id) = dns::provider_for(&ctx, &zone_id, &op.requester).await?;
    run_op(&ctx, op, || async {
        let change = p.create_record(&id, &input).await?;
        Ok((StatusCode::CREATED, change_value(&change)))
    })
    .await
}

/// `PATCH /api/v1/dns/zones/{zoneId}/records/{recordId}` → `{record, taskId?}`.
pub async fn update_record(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path((zone_id, record_id)): Path<(String, String)>,
    ApiJson(input): ApiJson<RecordInput>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers, scope::INFRA_WRITE).await?;
    let op = Op::new(
        &headers,
        &svc,
        provider_key(&zone_id),
        "record.update",
        format!("{zone_id} {record_id}"),
    )?;
    let (p, id) = dns::provider_for(&ctx, &zone_id, &op.requester).await?;
    run_op(&ctx, op, || async {
        let change = p.update_record(&id, &record_id, &input).await?;
        Ok((StatusCode::OK, change_value(&change)))
    })
    .await
}

/// `DELETE /api/v1/dns/zones/{zoneId}/records/{recordId}` → `{taskId?}`.
pub async fn delete_record(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path((zone_id, record_id)): Path<(String, String)>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers, scope::INFRA_WRITE).await?;
    let op = Op::new(
        &headers,
        &svc,
        provider_key(&zone_id),
        "record.delete",
        format!("{zone_id} {record_id}"),
    )?;
    let (p, id) = dns::provider_for(&ctx, &zone_id, &op.requester).await?;
    run_op(&ctx, op, || async {
        let task = p.delete_record(&id, &record_id).await?;
        let body = task.map_or_else(|| json!({}), |t| json!({ "taskId": t }));
        Ok((StatusCode::OK, body))
    })
    .await
}

/// `POST /api/v1/dns/zones/{zoneId}/records/ensure` → `{record, changed, taskId?}`.
pub async fn ensure_record(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(zone_id): Path<String>,
    ApiJson(input): ApiJson<RecordInput>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers, scope::INFRA_WRITE).await?;
    let op = Op::new(
        &headers,
        &svc,
        provider_key(&zone_id),
        "record.ensure",
        record_target(&zone_id, &input),
    )?;
    let (p, id) = dns::provider_for(&ctx, &zone_id, &op.requester).await?;
    run_op(&ctx, op, || async {
        let (change, changed) = dns::ensure(p.as_ref(), &id, &input).await?;
        let mut body = change_value(&change);
        body["changed"] = json!(changed);
        Ok((StatusCode::OK, body))
    })
    .await
}

/// `GET /api/v1/dns/servers` — agents acting as DNS servers (they reported a
/// DNS server, or one was installed through Nexus). `zones` is the number of
/// zones the agent reported serving.
pub async fn list_servers(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    operator(&ctx, &headers, scope::INFRA_READ).await?;
    let all = agents::Model::find_all(&ctx.db).await?;
    let out: Vec<Value> = all
        .iter()
        .filter_map(|a| {
            let dns_server = agents::json_column(a.dns_server.as_deref(), Value::Null);
            let software = dns_server
                .get("software")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if software.is_empty() && a.dns_install_task_id.is_none() {
                return None;
            }
            Some(json!({
                "agentId": a.agent_id.to_string(),
                "hostname": a.hostname,
                "state": super::gateway::agent_state(a),
                "software": software,
                "version": dns_server.get("version").and_then(Value::as_str).unwrap_or_default(),
                "running": dns_server.get("running").and_then(Value::as_bool).unwrap_or(false),
                "zones": dns_server.get("zones").and_then(Value::as_array).map_or(0, Vec::len),
                "installTaskId": a.dns_install_task_id.clone().unwrap_or_default(),
            }))
        })
        .collect();
    Ok(axum::Json(out).into_response())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallServerRequest {
    #[serde(default)]
    pub agent_id: Option<String>,
}

/// `POST /api/v1/dns/servers` `{agentId}` → dispatch `install_dns_server`.
pub async fn install_server(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    ApiJson(req): ApiJson<InstallServerRequest>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers, scope::INFRA_WRITE).await?;
    let raw = req
        .agent_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::invalid("agentId: required"))?;
    let agent_id = uuid::Uuid::parse_str(raw).map_err(|_| ApiError::not_found("no such agent"))?;
    let agent = agents::Model::find_by_agent_id(&ctx.db, &agent_id)
        .await
        .map_err(|_| ApiError::not_found("no such agent"))?;
    let op = Op::new(&headers, &svc, BIND, "server.install", agent_id.to_string())?;
    let who = op.requester.clone();
    run_op(&ctx, op, || async {
        let task = dispatch::dispatch(
            &ctx,
            &DispatchRequest {
                intent: INTENT_INSTALL_DNS_SERVER.to_string(),
                targets: vec![agent_id.to_string()],
                requester: who,
                auto_rollback: false,
                params: std::collections::BTreeMap::new(),
            },
        )
        .await?;
        let task_id = task.task_id.to_string();
        agent.set_dns_install_task(&ctx.db, &task_id).await?;
        Ok((
            StatusCode::OK,
            json!({ "agentId": agent_id.to_string(), "taskId": task_id }),
        ))
    })
    .await
}

pub fn routes() -> Routes {
    Routes::new()
        .prefix("api/v1")
        .add("/dns/zones", get(list_zones).post(create_zone))
        .add("/dns/zones/{zone_id}", get(get_zone).delete(delete_zone))
        .add(
            "/dns/zones/{zone_id}/records",
            get(list_records).post(create_record),
        )
        .add("/dns/zones/{zone_id}/records/ensure", post(ensure_record))
        .add(
            "/dns/zones/{zone_id}/records/{record_id}",
            patch(update_record).delete(delete_record),
        )
        .add("/dns/servers", get(list_servers).post(install_server))
}
