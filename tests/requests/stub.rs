//! In-process stand-ins for the services Nexus calls: the Orchestrator
//! (`/orch/plan`, echoing a plan whose single step carries the params),
//! Cloudflare (`/cf/...`, the v4 envelope) and DigitalOcean (`/v2/...`).
//! Every request is recorded so tests can assert on what Nexus sent.
//!
//! Environment variables are process-global; every test that uses this is
//! `#[serial]`, and [`Env`] restores what it changed when dropped.

use std::sync::{Arc, Mutex};

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json, Router,
};
use linexus_nexus::middleware::system_token;
use serde_json::{json, Value};

pub const CF_TOKEN: &str = "cf-test-token-5f1e2d3c4b5a";
/// An account-owned token: valid for zones, but `/user/tokens/verify` does not
/// know it and it may not list `/accounts` (it has no Account Settings: Read).
pub const CF_ACCT_TOKEN: &str = "cf-acct-token-9a8b7c6d5e4f";
pub const DO_TOKEN: &str = "dop_v1_0123456789abcdef0123456789abcdef";

/// Sets environment variables; restores the previous values on drop.
pub struct Env(Vec<(String, Option<String>)>);

impl Env {
    pub fn new() -> Self {
        let mut e = Self(Vec::new());
        // Never reach a real Orchestrator or Logger from a test.
        e.set("LINEXUS_ORCH_URL", "http://127.0.0.1:9");
        e.set("LINEXUS_LOGGER_URL", "http://127.0.0.1:9");
        for v in [
            "DIGITALOCEAN_TOKEN",
            "CLOUDFLARE_API_TOKEN",
            "CLOUDFLARE_ACCOUNT_ID",
            "DIGITALOCEAN_API_BASE",
            "CLOUDFLARE_API_BASE",
            "NEXUS_PUBLIC_URL",
            "LINEXUS_AGENT_BINARY_DIR",
            "DAEDALUS_INGEST_URL",
            "NEXUS_SIGNING_KEY",
            "NEXUS_PLAN_TTL_SECS",
            "NEXUS_REQUIRE_OPERATOR_CERT",
            "NEXUS_TLS_CERT",
            "NEXUS_TLS_KEY",
            "NEXUS_TLS_CLIENT_CA",
        ] {
            e.unset(v);
        }
        e.set("NEXUS_SECRET_KEY", "test-sealing-key");
        e
    }

    pub fn set(&mut self, k: &str, v: &str) {
        self.0.push((k.to_string(), std::env::var(k).ok()));
        std::env::set_var(k, v);
    }

    pub fn unset(&mut self, k: &str) {
        self.0.push((k.to_string(), std::env::var(k).ok()));
        std::env::remove_var(k);
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        for (k, v) in self.0.iter().rev() {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
}

pub fn bearer() -> (HeaderName, HeaderValue) {
    bearer_of(
        &std::env::var(system_token::ENV_ROOT_TOKEN)
            .unwrap_or_else(|_| system_token::DEV_ROOT_TOKEN.to_string()),
    )
}

pub fn bearer_of(token: &str) -> (HeaderName, HeaderValue) {
    (
        HeaderName::from_static("authorization"),
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    )
}

pub fn header(name: &'static str, value: &str) -> (HeaderName, HeaderValue) {
    (
        HeaderName::from_static(name),
        HeaderValue::from_str(value).unwrap(),
    )
}

/// One recorded request.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub body: Value,
}

#[derive(Default)]
pub struct StubState {
    pub seen: Vec<Seen>,
    pub zones: Vec<Value>,
    pub records: Vec<(String, Value)>,
    pub droplets: Vec<Value>,
    pub volumes: Vec<Value>,
    pub lbs: Vec<Value>,
    pub domains: Vec<Value>,
    pub snapshots: Vec<Value>,
    pub next: u64,
}

pub type Shared = Arc<Mutex<StubState>>;

/// A running stub; `base` is `http://127.0.0.1:<port>`.
pub struct Stub {
    pub base: String,
    pub state: Shared,
}

impl Stub {
    pub async fn start() -> Self {
        let state: Shared = Arc::default();
        state.lock().unwrap().domains.push(json!({
            "id": "dom1", "name": "example.com", "status": "active",
            "auto_renew": true, "locked": true, "privacy": true,
            "expires_at": "2027-10-09T00:00:00Z",
            "name_servers": ["ada.ns.cloudflare.com", "bob.ns.cloudflare.com"],
        }));
        let app = Router::new().fallback(handle).with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            base: format!("http://{addr}"),
            state,
        }
    }

    /// Point Nexus at this stub for the Orchestrator, Cloudflare and DigitalOcean.
    pub fn wire(&self, env: &mut Env) {
        env.set("LINEXUS_ORCH_URL", &format!("{}/orch", self.base));
        env.set("CLOUDFLARE_API_BASE", &format!("{}/cf", self.base));
        env.set("DIGITALOCEAN_API_BASE", &self.base);
    }

    pub fn seen(&self, method: &str, path: &str) -> Vec<Seen> {
        self.state
            .lock()
            .unwrap()
            .seen
            .iter()
            .filter(|s| s.method == method && s.path == path)
            .cloned()
            .collect()
    }
}

fn cf_ok(result: Value) -> Response {
    Json(json!({
        "success": true, "errors": [], "messages": [], "result": result,
        "result_info": {"page": 1, "per_page": 50, "total_pages": 1, "count": 1, "total_count": 1},
    }))
    .into_response()
}

fn cf_err(status: StatusCode, code: i64, message: &str) -> Response {
    (
        status,
        Json(json!({"success": false, "errors": [{"code": code, "message": message}], "result": null})),
    )
        .into_response()
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({"id": "not_found", "message": "The resource you were accessing could not be found."}))).into_response()
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| {
            v.replace("%40", "@")
                .replace("%3A", ":")
                .replace("%2A", "*")
        })
    })
}

async fn handle(
    State(st): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();
    let query = uri.query().unwrap_or_default().to_string();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let mut s = st.lock().unwrap();
    s.seen.push(Seen {
        method: method.to_string(),
        path: path.clone(),
        body: body.clone(),
    });
    s.next += 1;
    let n = s.next;
    let segs: Vec<&str> = path.trim_matches('/').split('/').collect();
    let m = method.as_str();

    match (m, segs.as_slice()) {
        // ---------------------------------------------------------- Orchestrator
        ("POST", ["orch", "plan"]) => {
            let task_id = body["task_id"].clone();
            Json(json!({
                "task_id": task_id,
                "status": "planned",
                "plan": {
                    "task_id": task_id,
                    "intent": body["intent"],
                    "targets": body["targets"],
                    "auto_rollback": body["auto_rollback"],
                    "steps": [{"id": "s1", "action": body["intent"], "params": body["params"], "critical": true}],
                },
            }))
            .into_response()
        }

        // ---------------------------------------------------------- Cloudflare
        (_, ["cf", ..])
            if auth != format!("Bearer {CF_TOKEN}") && auth != format!("Bearer {CF_ACCT_TOKEN}") =>
        {
            cf_err(StatusCode::FORBIDDEN, 9109, "Invalid access token")
        }
        ("GET", ["cf", "user", "tokens", "verify"]) if auth == format!("Bearer {CF_ACCT_TOKEN}") => {
            cf_err(StatusCode::UNAUTHORIZED, 1000, "Invalid API Token")
        }
        ("GET", ["cf", "accounts"]) if auth == format!("Bearer {CF_ACCT_TOKEN}") => {
            cf_err(StatusCode::FORBIDDEN, 9109, "Unauthorized to access requested resource")
        }
        ("GET", ["cf", "accounts", "acc1", "tokens", "verify"]) => {
            cf_ok(json!({"id": "tok", "status": "active"}))
        }
        ("GET", ["cf", "user", "tokens", "verify"]) => {
            cf_ok(json!({"id": "tok", "status": "active"}))
        }
        ("GET", ["cf", "accounts"]) => cf_ok(json!([{"id": "acc1", "name": "Acme Hosting"}])),
        ("GET", ["cf", "zones"]) => match query_param(&query, "account.id") {
            Some(a) if a != "acc1" => cf_err(
                StatusCode::BAD_REQUEST,
                70503,
                "account with given Tag doesn't exist",
            ),
            _ => cf_ok(Value::Array(s.zones.clone())),
        },
        ("POST", ["cf", "zones"]) => {
            if body["account"]["id"].as_str().is_some_and(|a| a != "acc1") {
                return cf_err(
                    StatusCode::BAD_REQUEST,
                    70503,
                    "account with given Tag doesn't exist",
                );
            }
            let name = body["name"].as_str().unwrap_or_default().to_string();
            if name == "leak.example.com" {
                // A provider that echoes the credential back in its error.
                return cf_err(
                    StatusCode::BAD_REQUEST,
                    1000,
                    &format!("rejected request carrying {auth}"),
                );
            }
            if s.zones.iter().any(|z| z["name"] == name) {
                return cf_err(
                    StatusCode::BAD_REQUEST,
                    1061,
                    &format!("{name} already exists"),
                );
            }
            let z = json!({
                "id": format!("{n:032x}"), "name": name, "status": "pending",
                "name_servers": ["ada.ns.cloudflare.com", "bob.ns.cloudflare.com"],
                "original_name_servers": ["ns1.registrar.example"],
                "created_on": "2026-10-09T00:00:00Z", "account": body["account"],
            });
            s.zones.push(z.clone());
            cf_ok(z)
        }
        ("GET", ["cf", "zones", id]) => match s.zones.iter().find(|z| z["id"] == *id) {
            Some(z) => cf_ok(z.clone()),
            None => cf_err(StatusCode::NOT_FOUND, 1001, "Invalid zone identifier"),
        },
        ("DELETE", ["cf", "zones", id]) => {
            let id = (*id).to_string();
            s.zones.retain(|z| z["id"] != id.as_str());
            cf_ok(json!({"id": id}))
        }
        ("GET", ["cf", "zones", zid, "dns_records"]) => {
            let t = query_param(&query, "type");
            let name = query_param(&query, "name");
            let out: Vec<Value> = s
                .records
                .iter()
                .filter(|(z, r)| {
                    z == zid
                        && t.as_ref().is_none_or(|t| r["type"] == t.as_str())
                        && name.as_ref().is_none_or(|nm| r["name"] == nm.as_str())
                })
                .map(|(_, r)| r.clone())
                .collect();
            cf_ok(Value::Array(out))
        }
        ("POST", ["cf", "zones", zid, "dns_records"]) => {
            let mut r = body.clone();
            r["id"] = json!(format!("{n:032x}"));
            s.records.push(((*zid).to_string(), r.clone()));
            cf_ok(r)
        }
        ("GET", ["cf", "zones", zid, "dns_records", rid]) => {
            match s.records.iter().find(|(z, r)| z == zid && r["id"] == *rid) {
                Some((_, r)) => cf_ok(r.clone()),
                None => cf_err(StatusCode::NOT_FOUND, 81044, "Record does not exist."),
            }
        }
        ("PATCH", ["cf", "zones", zid, "dns_records", rid]) => {
            let Some((_, r)) = s
                .records
                .iter_mut()
                .find(|(z, r)| z == zid && r["id"] == *rid)
            else {
                return cf_err(StatusCode::NOT_FOUND, 81044, "Record does not exist.");
            };
            if let (Value::Object(dst), Value::Object(src)) = (&mut *r, &body) {
                for (k, v) in src {
                    dst.insert(k.clone(), v.clone());
                }
            }
            cf_ok(r.clone())
        }
        ("DELETE", ["cf", "zones", zid, "dns_records", rid]) => {
            s.records.retain(|(z, r)| !(z == zid && r["id"] == *rid));
            cf_ok(json!({"id": rid}))
        }
        ("GET", ["cf", "accounts", "acc1", "registrar", "domains"]) => {
            cf_ok(Value::Array(s.domains.clone()))
        }
        ("GET", ["cf", "accounts", "acc1", "registrar", "domains", name]) => {
            match s.domains.iter().find(|d| d["name"] == *name) {
                Some(d) => cf_ok(d.clone()),
                None => cf_err(StatusCode::NOT_FOUND, 10000, "domain not found"),
            }
        }
        ("PUT", ["cf", "accounts", "acc1", "registrar", "domains", name]) => {
            let Some(d) = s.domains.iter_mut().find(|d| d["name"] == *name) else {
                return cf_err(StatusCode::NOT_FOUND, 10000, "domain not found");
            };
            if let (Value::Object(dst), Value::Object(src)) = (&mut *d, &body) {
                for (k, v) in src {
                    dst.insert(k.clone(), v.clone());
                }
            }
            cf_ok(d.clone())
        }

        // ---------------------------------------------------------- DigitalOcean
        (_, ["v2", ..]) if auth != format!("Bearer {DO_TOKEN}") => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"id": "Unauthorized", "message": "Unable to authenticate you"})),
        )
            .into_response(),
        ("GET", ["v2", "account"]) => Json(json!({"account": {
            "uuid": "acct-uuid", "email": "ops@example.com", "team": {"name": "Ops Team"},
            "droplet_limit": 25, "volume_limit": 100, "status": "active",
        }}))
        .into_response(),
        ("GET", ["v2", "customers", "my", "balance"]) => Json(json!({
            "month_to_date_balance": "12.34", "account_balance": "0.00",
            "month_to_date_usage": "12.34", "generated_at": "2026-10-09T00:00:00Z",
        }))
        .into_response(),
        ("GET", ["v2", "droplets"]) => {
            let all = s.droplets.clone();
            Json(json!({"droplets": all, "links": {}, "meta": {"total": all.len()}}))
                .into_response()
        }
        ("POST", ["v2", "droplets"]) => {
            let d = json!({
                "id": 1000 + n, "name": body["name"], "status": "new", "memory": 1024, "vcpus": 1,
                "disk": 25, "region": {"slug": body["region"]}, "size_slug": body["size"],
                "size": {"slug": body["size"], "price_monthly": 6.0},
                "image": {"slug": body["image"]}, "networks": {"v4": [], "v6": []},
                "tags": body["tags"], "volume_ids": [], "features": [],
                "created_at": "2026-10-09T00:00:00Z",
            });
            s.droplets.push(d.clone());
            (
                StatusCode::ACCEPTED,
                Json(json!({"droplet": d, "links": {"actions": []}})),
            )
                .into_response()
        }
        ("GET", ["v2", "droplets", id]) => {
            match s
                .droplets
                .iter()
                .find(|d| d["id"] == id.parse::<u64>().unwrap_or(0))
            {
                Some(d) => Json(json!({"droplet": d})).into_response(),
                None => not_found(),
            }
        }
        ("DELETE", ["v2", "droplets", id]) => {
            let id = id.parse::<u64>().unwrap_or(0);
            s.droplets.retain(|d| d["id"] != id);
            StatusCode::NO_CONTENT.into_response()
        }
        ("POST", ["v2", "droplets", id, "actions"]) => (
            StatusCode::CREATED,
            Json(json!({"action": {
                "id": 7000 + n, "type": body["type"], "status": "in-progress",
                "started_at": "2026-10-09T00:00:00Z", "completed_at": null,
                "resource_id": id.parse::<u64>().unwrap_or(0), "resource_type": "droplet",
            }})),
        )
            .into_response(),
        ("POST", ["v2", "volumes"]) => {
            let v = json!({
                "id": format!("{:08x}-0000-4000-8000-{:012x}", n, n), "name": body["name"],
                "region": {"slug": body["region"]}, "size_gigabytes": body["size_gigabytes"],
                "description": body["description"], "filesystem_type": body["filesystem_type"],
                "filesystem_label": "", "droplet_ids": [], "tags": body["tags"],
                "created_at": "2026-10-09T00:00:00Z",
            });
            s.volumes.push(v.clone());
            (StatusCode::CREATED, Json(json!({"volume": v}))).into_response()
        }
        ("GET", ["v2", "volumes", id]) => match s.volumes.iter().find(|v| v["id"] == *id) {
            Some(v) => Json(json!({"volume": v})).into_response(),
            None => not_found(),
        },
        ("GET", ["v2", "snapshots", id]) => match s.snapshots.iter().find(|v| v["id"].to_string().trim_matches('"') == *id) {
            Some(v) => Json(json!({"snapshot": v})).into_response(),
            None => not_found(),
        },
        ("DELETE", ["v2", "snapshots", id]) => {
            let before = s.snapshots.len();
            s.snapshots
                .retain(|v| v["id"].to_string().trim_matches('"') != *id);
            if s.snapshots.len() == before {
                return not_found();
            }
            StatusCode::NO_CONTENT.into_response()
        }
        ("GET", ["v2", "certificates"]) => Json(json!({
            "certificates": [{
                "id": "892071a0-bb95-49bc-8021-3afd67a210bf", "name": "web-cert-01",
                "not_after": "2027-02-22T00:23:00Z", "sha1_fingerprint": "dfcc9f57d86bf58e321c2c6c31c7a971be244ac7",
                "created_at": "2026-02-08T16:02:37Z", "dns_names": ["www.example.com", "example.com"],
                "state": "verified", "type": "lets_encrypt",
            }],
            "links": {}, "meta": {"total": 1},
        }))
        .into_response(),
        ("POST", ["v2", "load_balancers"]) => {
            let mut lb = body.clone();
            lb["id"] = json!(format!("{:08x}-1111-4000-8000-{:012x}", n, n));
            lb["ip"] = json!("203.0.113.50");
            lb["status"] = json!("new");
            lb["region"] = json!({"slug": body["region"]});
            lb["created_at"] = json!("2026-10-09T00:00:00Z");
            s.lbs.push(lb.clone());
            (StatusCode::ACCEPTED, Json(json!({"load_balancer": lb}))).into_response()
        }
        _ => not_found(),
    }
}
