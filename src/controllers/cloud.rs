//! # Cloud — DigitalOcean (`docs/PROVIDERS.md` §7)
//!
//! Droplets, volumes, load balancers, firewalls and VPCs. Every mutation goes
//! through the operations log (and `Idempotency-Key`); deletes need
//! `X-Confirm: <name>`. Ids from the path are parsed (droplets and actions are
//! integers, volumes and load balancers UUIDs) before they reach a URL.

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use loco_rs::prelude::{get, post, AppContext, Routes};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::api::{operator, require_confirm, run_op, ApiError, ApiJson, ApiResult, Op};
use super::enrollment::{mint_params, MintRequest};
use crate::dispatch::{self, DispatchRequest};
use crate::models::{agents, enrollment_tokens};
use crate::providers::{
    camelize,
    digitalocean::{self as dox, DigitalOcean},
    DIGITALOCEAN,
};

pub const INTENT_MOUNT_VOLUME: &str = "mount_volume";
const CATALOG_TTL: Duration = Duration::from_secs(3600);
const MAX_USER_DATA: usize = 64 * 1024;
const DROPLET_ACTIONS: [&str; 11] = [
    "power_on",
    "power_off",
    "shutdown",
    "reboot",
    "power_cycle",
    "resize",
    "snapshot",
    "rebuild",
    "rename",
    "enable_backups",
    "disable_backups",
];
const LB_PROTOCOLS: [&str; 6] = ["http", "https", "http2", "http3", "tcp", "udp"];

fn droplet_id(raw: &str) -> ApiResult<u64> {
    raw.trim()
        .parse::<u64>()
        .map_err(|_| ApiError::not_found(format!("no such droplet: {raw}")))
}

fn resource_uuid(raw: &str, what: &str) -> ApiResult<uuid::Uuid> {
    uuid::Uuid::parse_str(raw.trim())
        .map_err(|_| ApiError::not_found(format!("no such {what}: {raw}")))
}

/// A slug (region, size, image): lowercase letters, digits, dashes, dots.
fn valid_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.' || c == '_'
        })
}

/// A DigitalOcean resource name (droplet, load balancer): letters, digits,
/// dots and dashes.
fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

fn valid_tag(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_'))
}

fn tags_of(tags: Option<&Vec<String>>) -> ApiResult<Vec<String>> {
    let tags: Vec<String> = tags.cloned().unwrap_or_default();
    if let Some(bad) = tags.iter().find(|t| !valid_tag(t)) {
        return Err(ApiError::invalid(format!(
            "tags: `{bad}` (letters, digits, :, - and _)"
        )));
    }
    Ok(tags)
}

/// An image: a slug or a numeric id.
fn image_of(v: &Value, field: &str) -> ApiResult<Value> {
    match v {
        Value::String(s) if valid_slug(s.trim()) => Ok(json!(s.trim())),
        Value::String(s) if s.trim().parse::<u64>().is_ok() => {
            Ok(json!(s.trim().parse::<u64>().unwrap_or_default()))
        }
        Value::Number(n) if n.as_u64().is_some() => Ok(v.clone()),
        _ => Err(ApiError::invalid(format!("{field}: an image slug or id"))),
    }
}

async fn all_agents(ctx: &AppContext) -> ApiResult<Vec<agents::Model>> {
    Ok(agents::Model::find_all(&ctx.db).await?)
}

// ---------------------------------------------------------------------------
// Account and catalog
// ---------------------------------------------------------------------------

/// `GET /api/v1/cloud/account`.
pub async fn account(State(ctx): State<AppContext>, headers: HeaderMap) -> ApiResult<Response> {
    operator(&ctx, &headers).await?;
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    let a = do_.account().await?;
    // A token without billing scope still gets its account (balance null).
    let balance = do_.balance().await.ok();
    Ok(axum::Json(dox::account_json(&a, balance.as_ref())).into_response())
}

fn catalog_cache() -> &'static Mutex<Option<(String, Instant, Value)>> {
    static CACHE: OnceLock<Mutex<Option<(String, Instant, Value)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// `GET /api/v1/cloud/catalog` — regions, sizes, distribution images; cached 1 h.
pub async fn catalog(State(ctx): State<AppContext>, headers: HeaderMap) -> ApiResult<Response> {
    operator(&ctx, &headers).await?;
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    let key = do_.cache_key();
    let mut cache = catalog_cache().lock().await;
    if let Some((k, at, v)) = cache.as_ref() {
        if *k == key && at.elapsed() < CATALOG_TTL {
            return Ok(axum::Json(v.clone()).into_response());
        }
    }
    let regions = do_.list("/v2/regions", "regions", &[]).await?;
    let sizes = do_.list("/v2/sizes", "sizes", &[]).await?;
    let images = do_
        .list(
            "/v2/images",
            "images",
            &[("type", "distribution".to_string())],
        )
        .await?;
    let v = json!({
        "regions": regions.iter().map(dox::region_json).collect::<Vec<_>>(),
        "sizes": sizes.iter().map(dox::size_json).collect::<Vec<_>>(),
        "images": images.iter().map(dox::image_json).collect::<Vec<_>>(),
    });
    *cache = Some((key, Instant::now(), v.clone()));
    Ok(axum::Json(v).into_response())
}

// ---------------------------------------------------------------------------
// Droplets
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TagQuery {
    pub tag: Option<String>,
}

/// `GET /api/v1/cloud/droplets?tag=`.
pub async fn list_droplets(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Query(q): Query<TagQuery>,
) -> ApiResult<Response> {
    operator(&ctx, &headers).await?;
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    let mut query = Vec::new();
    if let Some(tag) = q.tag.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        if !valid_tag(tag) {
            return Err(ApiError::invalid("tag: letters, digits, :, - and _"));
        }
        query.push(("tag_name", tag.to_string()));
    }
    let droplets = do_.list("/v2/droplets", "droplets", &query).await?;
    let agents = all_agents(&ctx).await?;
    let out: Vec<Value> = droplets
        .iter()
        .map(|d| dox::droplet_json(d, &agents))
        .collect();
    Ok(axum::Json(out).into_response())
}

/// `GET /api/v1/cloud/droplets/{id}`.
pub async fn get_droplet(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    operator(&ctx, &headers).await?;
    let id = droplet_id(&id)?;
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    let d = do_.droplet(id).await?;
    Ok(axum::Json(dox::droplet_json(&d, &all_agents(&ctx).await?)).into_response())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollAgentSpec {
    #[serde(default)]
    pub hostgroup: Option<String>,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDroplet {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub image: Option<Value>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub vpc_uuid: Option<String>,
    #[serde(default)]
    pub ssh_keys: Option<Vec<Value>>,
    #[serde(default)]
    pub backups: Option<bool>,
    #[serde(default)]
    pub monitoring: Option<bool>,
    #[serde(default)]
    pub ipv6: Option<bool>,
    #[serde(default)]
    pub user_data: Option<String>,
    #[serde(default)]
    pub enroll_agent: Option<EnrollAgentSpec>,
}

fn required<'a>(v: Option<&'a String>, field: &str) -> ApiResult<&'a str> {
    v.map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::invalid(format!("{field}: required")))
}

/// The public URL droplets call home on (`NEXUS_PUBLIC_URL`), validated so
/// it can be written into a shell line as-is.
#[must_use]
pub fn public_url() -> Option<String> {
    std::env::var("NEXUS_PUBLIC_URL")
        .ok()
        .map(|u| u.trim().trim_end_matches('/').to_string())
        .filter(|u| {
            (u.starts_with("https://") || u.starts_with("http://"))
                && u.len() <= 512
                && u.chars().all(|c| {
                    c.is_ascii_alphanumeric() || matches!(c, ':' | '/' | '.' | '-' | '_' | '~')
                })
        })
}

/// `POST /api/v1/cloud/droplets` → `201 {droplet, enrollmentTokenId?}`.
///
/// With `enrollAgent`, a one-use, 24 h enrollment token is minted and the
/// droplet's `user_data` installs and enrolls the agent (the caller's own
/// `userData` runs after it); the droplet is tagged `lx-hostgroup-<hg>`.
pub async fn create_droplet(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    ApiJson(req): ApiJson<CreateDroplet>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers).await?;
    let name = required(req.name.as_ref(), "name")?.to_string();
    if !valid_name(&name) {
        return Err(ApiError::invalid("name: letters, digits, dots and dashes"));
    }
    let region = required(req.region.as_ref(), "region")?.to_string();
    let size = required(req.size.as_ref(), "size")?.to_string();
    if !valid_slug(&region) {
        return Err(ApiError::invalid("region: a region slug"));
    }
    if !valid_slug(&size) {
        return Err(ApiError::invalid("size: a size slug"));
    }
    let image = image_of(
        req.image
            .as_ref()
            .ok_or_else(|| ApiError::invalid("image: required"))?,
        "image",
    )?;
    let mut tags = tags_of(req.tags.as_ref())?;
    if let Some(v) = req.vpc_uuid.as_deref().filter(|v| !v.trim().is_empty()) {
        uuid::Uuid::parse_str(v.trim()).map_err(|_| ApiError::invalid("vpcUuid: a VPC UUID"))?;
    }
    let ssh_keys: Vec<Value> = req.ssh_keys.clone().unwrap_or_default();
    for k in &ssh_keys {
        let ok = k.as_u64().is_some()
            || k.as_str().is_some_and(|s| {
                !s.is_empty()
                    && s.len() <= 128
                    && s.chars()
                        .all(|c| c.is_ascii_hexdigit() || c == ':' || c.is_ascii_digit())
            });
        if !ok {
            return Err(ApiError::invalid("sshKeys: key ids or fingerprints"));
        }
    }
    let caller_user_data = req.user_data.clone().filter(|u| !u.trim().is_empty());
    if caller_user_data
        .as_ref()
        .is_some_and(|u| u.len() > MAX_USER_DATA - 2048)
    {
        return Err(ApiError::invalid("userData: at most 62 KiB"));
    }

    // Validate the enrollment before acting; the token is minted inside the
    // operation so an idempotent replay never mints a second one.
    let enroll = match &req.enroll_agent {
        Some(spec) => {
            let url = public_url().ok_or_else(|| {
                ApiError::invalid(
                    "enrollAgent: NEXUS_PUBLIC_URL is not set (or not a plain http(s) URL); the droplet could never call home",
                )
            })?;
            let params = mint_params(
                &MintRequest {
                    hostgroup: spec.hostgroup.clone(),
                    environment: spec.environment.clone(),
                    label: Some(format!("droplet {name}")),
                    ttl_minutes: Some(super::enrollment::DEFAULT_TTL_MINUTES),
                    max_uses: Some(1),
                    metadata: spec.metadata.clone(),
                },
                None,
            )
            .map_err(|e| ApiError::invalid(format!("enrollAgent.{}", e.detail)))?;
            let tag = dox::hostgroup_tag(&params.hostgroup);
            if !tags.contains(&tag) {
                tags.push(tag);
            }
            Some((url, params))
        }
        None => None,
    };

    let op = Op::new(&headers, &svc, DIGITALOCEAN, "droplet.create", name.clone())?;
    let requester = op.requester.clone();
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    run_op(&ctx, op, || async {
        let mut minted: Option<enrollment_tokens::Model> = None;
        let user_data = match enroll {
            Some((url, mut params)) => {
                params.created_by = Some(requester);
                let (row, plaintext) = enrollment_tokens::Model::mint(&ctx.db, &params).await?;
                minted = Some(row);
                Some(dox::enroll_user_data(
                    &url,
                    &plaintext,
                    caller_user_data.as_deref(),
                ))
            }
            None => caller_user_data,
        };
        let mut body = json!({
            "name": name,
            "region": region,
            "size": size,
            "image": image,
            "tags": tags,
            "ssh_keys": ssh_keys,
            "backups": req.backups.unwrap_or(false),
            "monitoring": req.monitoring.unwrap_or(true),
            "ipv6": req.ipv6.unwrap_or(false),
        });
        if let Some(v) = req
            .vpc_uuid
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            body["vpc_uuid"] = json!(v);
        }
        if let Some(u) = &user_data {
            body["user_data"] = json!(u);
        }
        let created = match do_.post("/v2/droplets", &body).await {
            Ok(c) => c,
            Err(e) => {
                // The droplet does not exist; its token must not outlive it.
                if let Some(t) = minted {
                    let _ = t.revoke(&ctx.db).await;
                }
                return Err(e.into());
            }
        };
        let droplet = created.get("droplet").cloned().unwrap_or(Value::Null);
        let mut out = json!({ "droplet": dox::droplet_json(&droplet, &[]) });
        if let Some(t) = minted {
            out["enrollmentTokenId"] = json!(t.token_id.to_string());
        }
        Ok((StatusCode::CREATED, out))
    })
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DropletAction {
    #[serde(rename = "type", default)]
    pub action_type: Option<String>,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub disk: Option<bool>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub image: Option<Value>,
}

/// `POST /api/v1/cloud/droplets/{id}/actions` → `{action}`.
pub async fn droplet_action(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<DropletAction>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers).await?;
    let id = droplet_id(&id)?;
    let t = required(req.action_type.as_ref(), "type")?.to_string();
    if !DROPLET_ACTIONS.contains(&t.as_str()) {
        return Err(ApiError::invalid(format!(
            "type: one of {}",
            DROPLET_ACTIONS.join(", ")
        )));
    }
    let mut body = json!({ "type": t });
    match t.as_str() {
        "resize" => {
            let size = required(req.size.as_ref(), "size")?;
            if !valid_slug(size) {
                return Err(ApiError::invalid("size: a size slug"));
            }
            body["size"] = json!(size);
            body["disk"] = json!(req.disk.unwrap_or(false));
        }
        "rebuild" => {
            body["image"] = image_of(
                req.image
                    .as_ref()
                    .ok_or_else(|| ApiError::invalid("image: required"))?,
                "image",
            )?;
        }
        "rename" => {
            let name = required(req.name.as_ref(), "name")?;
            if !valid_name(name) {
                return Err(ApiError::invalid("name: letters, digits, dots and dashes"));
            }
            body["name"] = json!(name);
        }
        "snapshot" => {
            if let Some(n) = req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                if n.len() > 255 || n.chars().any(char::is_control) {
                    return Err(ApiError::invalid("name: at most 255 printable characters"));
                }
                body["name"] = json!(n);
            }
        }
        _ => {}
    }
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    let op = Op::new(
        &headers,
        &svc,
        DIGITALOCEAN,
        "droplet.action",
        format!("{id} {t}"),
    )?;
    run_op(&ctx, op, || async {
        let r = do_
            .post(&format!("/v2/droplets/{id}/actions"), &body)
            .await?;
        let action = r.get("action").cloned().unwrap_or(Value::Null);
        Ok((
            StatusCode::OK,
            json!({ "action": dox::action_json(&action) }),
        ))
    })
    .await
}

/// `GET /api/v1/cloud/droplets/{id}/snapshots`.
pub async fn droplet_snapshots(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    operator(&ctx, &headers).await?;
    let id = droplet_id(&id)?;
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    let snaps = do_
        .list(&format!("/v2/droplets/{id}/snapshots"), "snapshots", &[])
        .await?;
    let out: Vec<Value> = snaps.iter().map(dox::snapshot_json).collect();
    Ok(axum::Json(out).into_response())
}

/// `DELETE /api/v1/cloud/droplets/{id}` — needs `X-Confirm: <droplet name>`.
pub async fn delete_droplet(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers).await?;
    let id = droplet_id(&id)?;
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    let d = do_.droplet(id).await?;
    let name = d
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    require_confirm(&headers, &name)?;
    let op = Op::new(
        &headers,
        &svc,
        DIGITALOCEAN,
        "droplet.delete",
        format!("{id} {name}"),
    )?;
    run_op(&ctx, op, || async {
        do_.delete(&format!("/v2/droplets/{id}"), None).await?;
        Ok((StatusCode::NO_CONTENT, Value::Null))
    })
    .await
}

/// `GET /api/v1/cloud/actions/{id}`.
pub async fn get_action(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    operator(&ctx, &headers).await?;
    let id: u64 = id
        .trim()
        .parse()
        .map_err(|_| ApiError::not_found("no such action"))?;
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    let r = do_.get(&format!("/v2/actions/{id}")).await?;
    let action = r.get("action").cloned().unwrap_or(Value::Null);
    Ok(axum::Json(dox::action_json(&action)).into_response())
}

// ---------------------------------------------------------------------------
// Volumes
// ---------------------------------------------------------------------------

/// `GET /api/v1/cloud/volumes`.
pub async fn list_volumes(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    operator(&ctx, &headers).await?;
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    let vols = do_.list("/v2/volumes", "volumes", &[]).await?;
    let out: Vec<Value> = vols.iter().map(dox::volume_json).collect();
    Ok(axum::Json(out).into_response())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateVolume {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub size_gigabytes: Option<i64>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub filesystem_type: Option<String>,
    #[serde(default)]
    pub filesystem_label: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

/// A volume name: lowercase letters, digits and dashes, starting with a letter.
fn valid_volume_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `POST /api/v1/cloud/volumes` → `201 Volume`.
pub async fn create_volume(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    ApiJson(req): ApiJson<CreateVolume>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers).await?;
    let name = required(req.name.as_ref(), "name")?.to_string();
    if !valid_volume_name(&name) {
        return Err(ApiError::invalid(
            "name: lowercase letters, digits and dashes, starting with a letter",
        ));
    }
    let region = required(req.region.as_ref(), "region")?.to_string();
    if !valid_slug(&region) {
        return Err(ApiError::invalid("region: a region slug"));
    }
    let size = req
        .size_gigabytes
        .ok_or_else(|| ApiError::invalid("sizeGigabytes: required"))?;
    if !(1..=16_384).contains(&size) {
        return Err(ApiError::invalid("sizeGigabytes: 1…16384"));
    }
    let fs = req
        .filesystem_type
        .as_deref()
        .map(|f| f.trim().to_lowercase())
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| "ext4".to_string());
    if !matches!(fs.as_str(), "ext4" | "xfs") {
        return Err(ApiError::invalid("filesystemType: ext4 or xfs"));
    }
    let mut body = json!({
        "name": name,
        "region": region,
        "size_gigabytes": size,
        "filesystem_type": fs,
        "tags": tags_of(req.tags.as_ref())?,
    });
    if let Some(d) = req
        .description
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        if d.len() > 1024 || d.chars().any(char::is_control) {
            return Err(ApiError::invalid(
                "description: at most 1024 printable characters",
            ));
        }
        body["description"] = json!(d);
    }
    if let Some(l) = req
        .filesystem_label
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        let max = if fs == "xfs" { 12 } else { 16 };
        if l.len() > max
            || !l
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(ApiError::invalid(format!(
                "filesystemLabel: at most {max} of letters, digits, - and _"
            )));
        }
        body["filesystem_label"] = json!(l);
    }
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    let op = Op::new(&headers, &svc, DIGITALOCEAN, "volume.create", name)?;
    run_op(&ctx, op, || async {
        let r = do_.post("/v2/volumes", &body).await?;
        let v = r.get("volume").cloned().unwrap_or(Value::Null);
        Ok((StatusCode::CREATED, dox::volume_json(&v)))
    })
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeAction {
    #[serde(rename = "type", default)]
    pub action_type: Option<String>,
    #[serde(default)]
    pub droplet_id: Option<u64>,
    #[serde(default)]
    pub size_gigabytes: Option<i64>,
}

/// `POST /api/v1/cloud/volumes/{id}/actions` → `{action}`.
pub async fn volume_action(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<VolumeAction>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers).await?;
    let id = resource_uuid(&id, "volume")?;
    let t = required(req.action_type.as_ref(), "type")?.to_string();
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    let vol = do_.volume(&id).await?;
    let region = vol
        .pointer("/region/slug")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let body = match t.as_str() {
        "attach" | "detach" => {
            let droplet = req
                .droplet_id
                .ok_or_else(|| ApiError::invalid("dropletId: required"))?;
            json!({ "type": t, "droplet_id": droplet, "region": region })
        }
        "resize" => {
            let size = req
                .size_gigabytes
                .ok_or_else(|| ApiError::invalid("sizeGigabytes: required"))?;
            if !(1..=16_384).contains(&size) {
                return Err(ApiError::invalid("sizeGigabytes: 1…16384"));
            }
            json!({ "type": t, "size_gigabytes": size, "region": region })
        }
        _ => return Err(ApiError::invalid("type: attach, detach or resize")),
    };
    let op = Op::new(
        &headers,
        &svc,
        DIGITALOCEAN,
        "volume.action",
        format!("{id} {t}"),
    )?;
    run_op(&ctx, op, || async {
        let r = do_
            .post(&format!("/v2/volumes/{id}/actions"), &body)
            .await?;
        let action = r.get("action").cloned().unwrap_or(Value::Null);
        Ok((
            StatusCode::OK,
            json!({ "action": dox::action_json(&action) }),
        ))
    })
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountVolume {
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub mount_point: Option<String>,
}

/// A mount point: absolute, plain characters, no `..`, not a system path.
fn valid_mount_point(p: &str) -> bool {
    const SYSTEM: [&str; 14] = [
        "/", "/bin", "/boot", "/dev", "/etc", "/lib", "/lib64", "/proc", "/root", "/run", "/sbin",
        "/sys", "/usr", "/var",
    ];
    p.starts_with('/')
        && p.len() <= 255
        && !p.split('/').any(|seg| seg == ".." || seg == ".")
        && p.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
        && !SYSTEM.contains(&match p.trim_end_matches('/') {
            "" => "/",
            t => t,
        })
        && !["/proc/", "/sys/", "/dev/", "/boot/"]
            .iter()
            .any(|pre| p.starts_with(pre))
}

/// `POST /api/v1/cloud/volumes/{id}/mount` `{agentId, mountPoint}` → `{taskId}`.
pub async fn mount_volume(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<MountVolume>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers).await?;
    let id = resource_uuid(&id, "volume")?;
    let agent_raw = required(req.agent_id.as_ref(), "agentId")?;
    let agent_id =
        uuid::Uuid::parse_str(agent_raw).map_err(|_| ApiError::not_found("no such agent"))?;
    agents::Model::find_by_agent_id(&ctx.db, &agent_id)
        .await
        .map_err(|_| ApiError::not_found("no such agent"))?;
    let mount_point = required(req.mount_point.as_ref(), "mountPoint")?.to_string();
    if !valid_mount_point(&mount_point) {
        return Err(ApiError::invalid(
            "mountPoint: an absolute path of letters, digits, /, ., _ and -, outside system directories",
        ));
    }
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    let vol = do_.volume(&id).await?;
    let name = vol
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !valid_volume_name(&name) {
        return Err(ApiError::invalid(
            "the volume's name cannot be used as a device path",
        ));
    }
    let fs = vol
        .get("filesystem_type")
        .and_then(Value::as_str)
        .filter(|f| matches!(*f, "ext4" | "xfs"))
        .unwrap_or("ext4")
        .to_string();
    let op = Op::new(
        &headers,
        &svc,
        DIGITALOCEAN,
        "volume.mount",
        format!("{id} {agent_id}:{mount_point}"),
    )?;
    let who = op.requester.clone();
    run_op(&ctx, op, || async {
        let mut params = BTreeMap::new();
        params.insert(
            "device".to_string(),
            format!("/dev/disk/by-id/scsi-0DO_Volume_{name}"),
        );
        params.insert("mountPoint".to_string(), mount_point);
        params.insert("fsType".to_string(), fs);
        params.insert("format".to_string(), "if_blank".to_string());
        let task = dispatch::dispatch(
            &ctx,
            &DispatchRequest {
                intent: INTENT_MOUNT_VOLUME.to_string(),
                targets: vec![agent_id.to_string()],
                requester: who,
                auto_rollback: false,
                params,
            },
        )
        .await?;
        Ok((
            StatusCode::OK,
            json!({ "taskId": task.task_id.to_string() }),
        ))
    })
    .await
}

/// `DELETE /api/v1/cloud/volumes/{id}` — needs `X-Confirm: <volume name>`.
pub async fn delete_volume(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers).await?;
    let id = resource_uuid(&id, "volume")?;
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    let vol = do_.volume(&id).await?;
    let name = vol
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    require_confirm(&headers, &name)?;
    let op = Op::new(
        &headers,
        &svc,
        DIGITALOCEAN,
        "volume.delete",
        format!("{id} {name}"),
    )?;
    run_op(&ctx, op, || async {
        do_.delete(&format!("/v2/volumes/{id}"), None).await?;
        Ok((StatusCode::NO_CONTENT, Value::Null))
    })
    .await
}

// ---------------------------------------------------------------------------
// Load balancers
// ---------------------------------------------------------------------------

/// `GET /api/v1/cloud/load-balancers`.
pub async fn list_lbs(State(ctx): State<AppContext>, headers: HeaderMap) -> ApiResult<Response> {
    operator(&ctx, &headers).await?;
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    let lbs = do_
        .list("/v2/load_balancers", "load_balancers", &[])
        .await?;
    let out: Vec<Value> = lbs.iter().map(dox::load_balancer_json).collect();
    Ok(axum::Json(out).into_response())
}

/// `GET /api/v1/cloud/load-balancers/{id}`.
pub async fn get_lb(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    operator(&ctx, &headers).await?;
    let id = resource_uuid(&id, "load balancer")?;
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    Ok(axum::Json(dox::load_balancer_json(&do_.load_balancer(&id).await?)).into_response())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardingRule {
    #[serde(default)]
    pub entry_protocol: Option<String>,
    #[serde(default)]
    pub entry_port: Option<i64>,
    #[serde(default)]
    pub target_protocol: Option<String>,
    #[serde(default)]
    pub target_port: Option<i64>,
    #[serde(default)]
    pub certificate_id: Option<String>,
    #[serde(default)]
    pub tls_passthrough: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheck {
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub port: Option<i64>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub check_interval_seconds: Option<i64>,
    #[serde(default)]
    pub response_timeout_seconds: Option<i64>,
    #[serde(default)]
    pub healthy_threshold: Option<i64>,
    #[serde(default)]
    pub unhealthy_threshold: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadBalancerSpec {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub size_unit: Option<i64>,
    #[serde(default)]
    pub forwarding_rules: Option<Vec<ForwardingRule>>,
    #[serde(default)]
    pub health_check: Option<HealthCheck>,
    #[serde(default)]
    pub droplet_ids: Option<Vec<u64>>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub vpc_uuid: Option<String>,
    #[serde(default)]
    pub redirect_http_to_https: Option<bool>,
}

fn port(p: Option<i64>, field: &str) -> ApiResult<i64> {
    match p {
        Some(p) if (1..=65_535).contains(&p) => Ok(p),
        _ => Err(ApiError::invalid(format!("{field}: 1…65535"))),
    }
}

fn protocol(p: Option<&String>, field: &str, allowed: &[&str]) -> ApiResult<String> {
    let p = required(p, field)?.to_lowercase();
    if allowed.contains(&p.as_str()) {
        Ok(p)
    } else {
        Err(ApiError::invalid(format!(
            "{field}: one of {}",
            allowed.join(", ")
        )))
    }
}

/// The DigitalOcean body for a load balancer definition.
fn lb_body(spec: &LoadBalancerSpec) -> ApiResult<(String, Value)> {
    let name = required(spec.name.as_ref(), "name")?.to_string();
    if !valid_name(&name) {
        return Err(ApiError::invalid("name: letters, digits, dots and dashes"));
    }
    let region = required(spec.region.as_ref(), "region")?.to_string();
    if !valid_slug(&region) {
        return Err(ApiError::invalid("region: a region slug"));
    }
    let size_unit = spec.size_unit.unwrap_or(1);
    if !(1..=100).contains(&size_unit) {
        return Err(ApiError::invalid("sizeUnit: 1…100"));
    }
    let rules = spec
        .forwarding_rules
        .as_ref()
        .filter(|r| !r.is_empty())
        .ok_or_else(|| ApiError::invalid("forwardingRules: at least one rule"))?;
    let mut do_rules = Vec::with_capacity(rules.len());
    for (i, r) in rules.iter().enumerate() {
        let f = |k: &str| format!("forwardingRules[{i}].{k}");
        let mut rule = json!({
            "entry_protocol": protocol(r.entry_protocol.as_ref(), &f("entryProtocol"), &LB_PROTOCOLS)?,
            "entry_port": port(r.entry_port, &f("entryPort"))?,
            "target_protocol": protocol(r.target_protocol.as_ref(), &f("targetProtocol"), &LB_PROTOCOLS)?,
            "target_port": port(r.target_port, &f("targetPort"))?,
            "tls_passthrough": r.tls_passthrough.unwrap_or(false),
        });
        if let Some(c) = r
            .certificate_id
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
        {
            uuid::Uuid::parse_str(c).map_err(|_| {
                ApiError::invalid(format!("{}: a certificate UUID", f("certificateId")))
            })?;
            rule["certificate_id"] = json!(c);
        }
        do_rules.push(rule);
    }
    let mut body = json!({
        "name": name,
        "region": region,
        "size_unit": size_unit,
        "forwarding_rules": do_rules,
        "redirect_http_to_https": spec.redirect_http_to_https.unwrap_or(false),
    });
    if let Some(h) = &spec.health_check {
        let mut hc = json!({
            "protocol": protocol(h.protocol.as_ref(), "healthCheck.protocol", &["http", "https", "tcp"])?,
            "port": port(h.port, "healthCheck.port")?,
        });
        if let Some(p) = h.path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            if !p.starts_with('/') || p.len() > 512 || p.chars().any(|c| c.is_control() || c == ' ')
            {
                return Err(ApiError::invalid("healthCheck.path: an absolute URL path"));
            }
            hc["path"] = json!(p);
        }
        for (k, v, lo, hi) in [
            ("check_interval_seconds", h.check_interval_seconds, 3, 300),
            (
                "response_timeout_seconds",
                h.response_timeout_seconds,
                3,
                300,
            ),
            ("healthy_threshold", h.healthy_threshold, 2, 10),
            ("unhealthy_threshold", h.unhealthy_threshold, 2, 10),
        ] {
            if let Some(v) = v {
                if !(lo..=hi).contains(&v) {
                    return Err(ApiError::invalid(format!("healthCheck.{k}: {lo}…{hi}")));
                }
                hc[k] = json!(v);
            }
        }
        body["health_check"] = hc;
    }
    let tag = spec.tag.as_deref().map(str::trim).filter(|t| !t.is_empty());
    match (&spec.droplet_ids, tag) {
        (Some(ids), Some(_)) if !ids.is_empty() => {
            return Err(ApiError::invalid("dropletIds and tag are exclusive"));
        }
        (_, Some(t)) => {
            if !valid_tag(t) {
                return Err(ApiError::invalid("tag: letters, digits, :, - and _"));
            }
            body["tag"] = json!(t);
        }
        (Some(ids), None) => body["droplet_ids"] = json!(ids),
        (None, None) => {}
    }
    if let Some(v) = spec
        .vpc_uuid
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        uuid::Uuid::parse_str(v).map_err(|_| ApiError::invalid("vpcUuid: a VPC UUID"))?;
        body["vpc_uuid"] = json!(v);
    }
    Ok((name, body))
}

/// `POST /api/v1/cloud/load-balancers` → `201 LoadBalancer`.
pub async fn create_lb(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    ApiJson(spec): ApiJson<LoadBalancerSpec>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers).await?;
    let (name, body) = lb_body(&spec)?;
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    let op = Op::new(&headers, &svc, DIGITALOCEAN, "load_balancer.create", name)?;
    run_op(&ctx, op, || async {
        let r = do_.post("/v2/load_balancers", &body).await?;
        let lb = r.get("load_balancer").cloned().unwrap_or(Value::Null);
        Ok((StatusCode::CREATED, dox::load_balancer_json(&lb)))
    })
    .await
}

/// `PUT /api/v1/cloud/load-balancers/{id}` — the whole definition.
pub async fn update_lb(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(spec): ApiJson<LoadBalancerSpec>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers).await?;
    let id = resource_uuid(&id, "load balancer")?;
    let (name, body) = lb_body(&spec)?;
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    let op = Op::new(
        &headers,
        &svc,
        DIGITALOCEAN,
        "load_balancer.update",
        format!("{id} {name}"),
    )?;
    run_op(&ctx, op, || async {
        let r = do_.put(&format!("/v2/load_balancers/{id}"), &body).await?;
        let lb = r.get("load_balancer").cloned().unwrap_or(Value::Null);
        Ok((StatusCode::OK, dox::load_balancer_json(&lb)))
    })
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LbDroplets {
    #[serde(default)]
    pub droplet_ids: Option<Vec<u64>>,
}

async fn lb_droplets(
    ctx: AppContext,
    headers: HeaderMap,
    id: String,
    req: LbDroplets,
    add: bool,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers).await?;
    let id = resource_uuid(&id, "load balancer")?;
    let ids = req
        .droplet_ids
        .filter(|i| !i.is_empty())
        .ok_or_else(|| ApiError::invalid("dropletIds: at least one droplet id"))?;
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    let operation = if add {
        "load_balancer.add_droplets"
    } else {
        "load_balancer.remove_droplets"
    };
    let op = Op::new(
        &headers,
        &svc,
        DIGITALOCEAN,
        operation,
        format!("{id} {ids:?}"),
    )?;
    run_op(&ctx, op, || async {
        let path = format!("/v2/load_balancers/{id}/droplets");
        let body = json!({ "droplet_ids": ids });
        if add {
            do_.post(&path, &body).await?;
        } else {
            do_.delete(&path, Some(&body)).await?;
        }
        Ok((StatusCode::NO_CONTENT, Value::Null))
    })
    .await
}

/// `POST /api/v1/cloud/load-balancers/{id}/droplets` → `204`.
pub async fn add_lb_droplets(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<LbDroplets>,
) -> ApiResult<Response> {
    lb_droplets(ctx, headers, id, req, true).await
}

/// `DELETE /api/v1/cloud/load-balancers/{id}/droplets` → `204`.
pub async fn remove_lb_droplets(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<LbDroplets>,
) -> ApiResult<Response> {
    lb_droplets(ctx, headers, id, req, false).await
}

/// `DELETE /api/v1/cloud/load-balancers/{id}` — needs `X-Confirm: <lb name>`.
pub async fn delete_lb(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let svc = operator(&ctx, &headers).await?;
    let id = resource_uuid(&id, "load balancer")?;
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    let lb = do_.load_balancer(&id).await?;
    let name = lb
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    require_confirm(&headers, &name)?;
    let op = Op::new(
        &headers,
        &svc,
        DIGITALOCEAN,
        "load_balancer.delete",
        format!("{id} {name}"),
    )?;
    run_op(&ctx, op, || async {
        do_.delete(&format!("/v2/load_balancers/{id}"), None)
            .await?;
        Ok((StatusCode::NO_CONTENT, Value::Null))
    })
    .await
}

// ---------------------------------------------------------------------------
// Read-only lists
// ---------------------------------------------------------------------------

/// `GET /api/v1/cloud/firewalls` — DigitalOcean's objects, keys camelCased.
pub async fn list_firewalls(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    operator(&ctx, &headers).await?;
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    let items = do_.list("/v2/firewalls", "firewalls", &[]).await?;
    Ok(axum::Json(items.iter().map(camelize).collect::<Vec<_>>()).into_response())
}

/// `GET /api/v1/cloud/vpcs` — DigitalOcean's objects, keys camelCased.
pub async fn list_vpcs(State(ctx): State<AppContext>, headers: HeaderMap) -> ApiResult<Response> {
    operator(&ctx, &headers).await?;
    let do_ = DigitalOcean::from_ctx(&ctx).await?;
    let items = do_.list("/v2/vpcs", "vpcs", &[]).await?;
    Ok(axum::Json(items.iter().map(camelize).collect::<Vec<_>>()).into_response())
}

pub fn routes() -> Routes {
    Routes::new()
        .prefix("api/v1")
        .add("/cloud/account", get(account))
        .add("/cloud/catalog", get(catalog))
        .add("/cloud/droplets", get(list_droplets).post(create_droplet))
        .add(
            "/cloud/droplets/{id}",
            get(get_droplet).delete(delete_droplet),
        )
        .add("/cloud/droplets/{id}/actions", post(droplet_action))
        .add("/cloud/droplets/{id}/snapshots", get(droplet_snapshots))
        .add("/cloud/actions/{id}", get(get_action))
        .add("/cloud/volumes", get(list_volumes).post(create_volume))
        .add("/cloud/volumes/{id}", axum::routing::delete(delete_volume))
        .add("/cloud/volumes/{id}/actions", post(volume_action))
        .add("/cloud/volumes/{id}/mount", post(mount_volume))
        .add("/cloud/load-balancers", get(list_lbs).post(create_lb))
        .add(
            "/cloud/load-balancers/{id}",
            get(get_lb).put(update_lb).delete(delete_lb),
        )
        .add(
            "/cloud/load-balancers/{id}/droplets",
            post(add_lb_droplets).delete(remove_lb_droplets),
        )
        .add("/cloud/firewalls", get(list_firewalls))
        .add("/cloud/vpcs", get(list_vpcs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_points() {
        assert!(valid_mount_point("/mnt/data"));
        assert!(valid_mount_point("/var/lib/postgresql"));
        assert!(!valid_mount_point("/"));
        assert!(!valid_mount_point("/etc"));
        assert!(!valid_mount_point("/mnt/../etc"));
        assert!(!valid_mount_point("mnt"));
        assert!(!valid_mount_point("/proc/x"));
        assert!(!valid_mount_point("/mnt/a b"));
    }
}
