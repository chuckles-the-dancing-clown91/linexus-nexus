//! DigitalOcean API v2 client (<https://docs.digitalocean.com/reference/api/>)
//! and the projections to the shapes in `docs/PROVIDERS.md` §7.
//!
//! Every path is built from validated ids (droplet and action ids are
//! integers; volume and load balancer ids are UUIDs), never from free text.

use loco_rs::app::AppContext;
use reqwest::Method;
use serde_json::{json, Value};

use super::{require_credentials, Api, Credentials, ProviderError, ProviderResult, DIGITALOCEAN};
use crate::models::agents;

const PER_PAGE: usize = 200;
const MAX_PAGES: usize = 100;

/// A DigitalOcean client bound to one token.
#[derive(Clone)]
pub struct DigitalOcean {
    api: Api,
}

impl DigitalOcean {
    /// From resolved credentials.
    pub fn new(creds: &Credentials) -> ProviderResult<Self> {
        Ok(Self {
            api: Api::new(DIGITALOCEAN, creds.token.clone())?,
        })
    }

    /// From the configured credentials, or `NotConfigured`.
    pub async fn from_ctx(ctx: &AppContext) -> ProviderResult<Self> {
        Self::new(&require_credentials(ctx, DIGITALOCEAN).await?)
    }

    /// A key identifying this client's base and token (for caches); the token
    /// itself is only present as a hash.
    #[must_use]
    pub fn cache_key(&self) -> String {
        format!(
            "{}|{}",
            super::api_base(DIGITALOCEAN),
            crate::models::system_tokens::hash_token(self.api.token())
        )
    }

    pub async fn get(&self, path: &str) -> ProviderResult<Value> {
        Ok(self.api.send(Method::GET, path, &[], None).await?.body)
    }

    pub async fn post(&self, path: &str, body: &Value) -> ProviderResult<Value> {
        Ok(self
            .api
            .send(Method::POST, path, &[], Some(body))
            .await?
            .body)
    }

    pub async fn put(&self, path: &str, body: &Value) -> ProviderResult<Value> {
        Ok(self
            .api
            .send(Method::PUT, path, &[], Some(body))
            .await?
            .body)
    }

    pub async fn delete(&self, path: &str, body: Option<&Value>) -> ProviderResult<()> {
        self.api.send(Method::DELETE, path, &[], body).await?;
        Ok(())
    }

    /// GET every page of a list endpoint, returning the items under `key`.
    pub async fn list(
        &self,
        path: &str,
        key: &str,
        query: &[(&str, String)],
    ) -> ProviderResult<Vec<Value>> {
        let mut out = Vec::new();
        for page in 1..=MAX_PAGES {
            let mut q: Vec<(&str, String)> = query.to_vec();
            q.push(("page", page.to_string()));
            q.push(("per_page", PER_PAGE.to_string()));
            let body = self.api.send(Method::GET, path, &q, None).await?.body;
            let items = body
                .get(key)
                .and_then(Value::as_array)
                .cloned()
                .ok_or_else(|| {
                    ProviderError::Unreachable(format!(
                        "digitalocean GET {path}: answer has no `{key}` list"
                    ))
                })?;
            let n = items.len();
            out.extend(items);
            let total = body
                .pointer("/meta/total")
                .and_then(Value::as_u64)
                .and_then(|t| usize::try_from(t).ok());
            let has_next = body
                .pointer("/links/pages/next")
                .is_some_and(Value::is_string);
            let done = match total {
                Some(t) => out.len() >= t,
                None => !has_next,
            };
            if n == 0 || done {
                break;
            }
        }
        Ok(out)
    }

    /// `/v2/account` → the `account` object.
    pub async fn account(&self) -> ProviderResult<Value> {
        let body = self.get("/v2/account").await?;
        body.get("account").cloned().ok_or_else(|| {
            ProviderError::Unreachable("digitalocean /v2/account: no `account`".into())
        })
    }

    /// `/v2/customers/my/balance`.
    pub async fn balance(&self) -> ProviderResult<Value> {
        self.get("/v2/customers/my/balance").await
    }

    pub async fn droplet(&self, id: u64) -> ProviderResult<Value> {
        let body = self.get(&format!("/v2/droplets/{id}")).await?;
        body.get("droplet")
            .cloned()
            .ok_or_else(|| ProviderError::Unreachable("digitalocean: no `droplet`".into()))
    }

    pub async fn volume(&self, id: &uuid::Uuid) -> ProviderResult<Value> {
        let body = self.get(&format!("/v2/volumes/{id}")).await?;
        body.get("volume")
            .cloned()
            .ok_or_else(|| ProviderError::Unreachable("digitalocean: no `volume`".into()))
    }

    pub async fn load_balancer(&self, id: &uuid::Uuid) -> ProviderResult<Value> {
        let body = self.get(&format!("/v2/load_balancers/{id}")).await?;
        body.get("load_balancer")
            .cloned()
            .ok_or_else(|| ProviderError::Unreachable("digitalocean: no `load_balancer`".into()))
    }
}

// ---------------------------------------------------------------------------
// Projections
// ---------------------------------------------------------------------------

fn s(v: &Value, ptr: &str) -> String {
    v.pointer(ptr)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn n(v: &Value, ptr: &str) -> Value {
    v.pointer(ptr)
        .cloned()
        .filter(Value::is_number)
        .unwrap_or(json!(0))
}

fn arr(v: &Value, ptr: &str) -> Value {
    v.pointer(ptr)
        .cloned()
        .filter(Value::is_array)
        .unwrap_or(json!([]))
}

fn opt_time(v: &Value, ptr: &str) -> Value {
    v.pointer(ptr)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map_or(Value::Null, |s| json!(s))
}

fn network(d: &Value, family: &str, kind: &str) -> String {
    d.pointer(&format!("/networks/{family}"))
        .and_then(Value::as_array)
        .and_then(|nets| {
            nets.iter()
                .find(|n| n.get("type").and_then(Value::as_str) == Some(kind))
        })
        .and_then(|n| n.get("ip_address").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

/// The tag that names the agent on a droplet.
pub const AGENT_TAG_PREFIX: &str = "lx-agent-";
/// The tag that names a droplet's hostgroup.
pub const HOSTGROUP_TAG_PREFIX: &str = "lx-hostgroup-";

/// A DigitalOcean tag for `hostgroup`: allowed characters kept, others `-`.
#[must_use]
pub fn hostgroup_tag(hostgroup: &str) -> String {
    let clean: String = hostgroup
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let mut tag = format!("{HOSTGROUP_TAG_PREFIX}{clean}");
    tag.truncate(255);
    tag
}

/// Which agent runs on droplet `d`: its `lx-agent-<id>` tag, else an agent
/// that reported the droplet's public or private IPv4.
#[must_use]
pub fn agent_for_droplet(d: &Value, all: &[agents::Model]) -> String {
    if let Some(tags) = d.get("tags").and_then(Value::as_array) {
        for t in tags.iter().filter_map(Value::as_str) {
            if let Some(id) = t.strip_prefix(AGENT_TAG_PREFIX) {
                if let Ok(uuid) = uuid::Uuid::parse_str(id) {
                    return uuid.to_string();
                }
            }
        }
    }
    let ips: Vec<String> = [network(d, "v4", "public"), network(d, "v4", "private")]
        .into_iter()
        .filter(|ip| !ip.is_empty())
        .collect();
    if ips.is_empty() {
        return String::new();
    }
    all.iter()
        .find(|a| a.addresses().iter().any(|ip| ips.contains(ip)))
        .map(|a| a.agent_id.to_string())
        .unwrap_or_default()
}

/// The `Droplet` shape.
#[must_use]
pub fn droplet_json(d: &Value, all: &[agents::Model]) -> Value {
    let image = {
        let slug = s(d, "/image/slug");
        if slug.is_empty() {
            let dist = s(d, "/image/distribution");
            let name = s(d, "/image/name");
            format!("{dist} {name}").trim().to_string()
        } else {
            slug
        }
    };
    let size = {
        let slug = s(d, "/size_slug");
        if slug.is_empty() {
            s(d, "/size/slug")
        } else {
            slug
        }
    };
    json!({
        "id": n(d, "/id"),
        "name": s(d, "/name"),
        "status": s(d, "/status"),
        "region": s(d, "/region/slug"),
        "size": size,
        "image": image,
        "memoryMb": n(d, "/memory"),
        "vcpus": n(d, "/vcpus"),
        "diskGb": n(d, "/disk"),
        "publicIpv4": network(d, "v4", "public"),
        "privateIpv4": network(d, "v4", "private"),
        "ipv6": network(d, "v6", "public"),
        "vpcUuid": s(d, "/vpc_uuid"),
        "tags": arr(d, "/tags"),
        "volumeIds": arr(d, "/volume_ids"),
        "features": arr(d, "/features"),
        "priceMonthly": n(d, "/size/price_monthly"),
        "createdAt": s(d, "/created_at"),
        "agentId": agent_for_droplet(d, all),
    })
}

/// The `action` shape.
#[must_use]
pub fn action_json(a: &Value) -> Value {
    json!({
        "id": n(a, "/id"),
        "type": s(a, "/type"),
        "status": s(a, "/status"),
        "startedAt": opt_time(a, "/started_at"),
        "completedAt": opt_time(a, "/completed_at"),
        "resourceId": n(a, "/resource_id"),
        "resourceType": s(a, "/resource_type"),
    })
}

/// The `Volume` shape.
#[must_use]
pub fn volume_json(v: &Value) -> Value {
    json!({
        "id": s(v, "/id"),
        "name": s(v, "/name"),
        "region": s(v, "/region/slug"),
        "sizeGigabytes": n(v, "/size_gigabytes"),
        "description": s(v, "/description"),
        "filesystemType": s(v, "/filesystem_type"),
        "filesystemLabel": s(v, "/filesystem_label"),
        "dropletIds": arr(v, "/droplet_ids"),
        "tags": arr(v, "/tags"),
        "createdAt": s(v, "/created_at"),
    })
}

/// The `LoadBalancer` shape.
#[must_use]
pub fn load_balancer_json(lb: &Value) -> Value {
    let rules: Vec<Value> = lb
        .get("forwarding_rules")
        .and_then(Value::as_array)
        .map(|rs| {
            rs.iter()
                .map(|r| {
                    json!({
                        "entryProtocol": s(r, "/entry_protocol"),
                        "entryPort": n(r, "/entry_port"),
                        "targetProtocol": s(r, "/target_protocol"),
                        "targetPort": n(r, "/target_port"),
                        "certificateId": s(r, "/certificate_id"),
                        "tlsPassthrough": r.get("tls_passthrough").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let hc = lb
        .get("health_check")
        .filter(|v| v.is_object())
        .map_or(Value::Null, |h| {
            json!({
                "protocol": s(h, "/protocol"),
                "port": n(h, "/port"),
                "path": s(h, "/path"),
                "checkIntervalSeconds": n(h, "/check_interval_seconds"),
                "responseTimeoutSeconds": n(h, "/response_timeout_seconds"),
                "healthyThreshold": n(h, "/healthy_threshold"),
                "unhealthyThreshold": n(h, "/unhealthy_threshold"),
            })
        });
    json!({
        "id": s(lb, "/id"),
        "name": s(lb, "/name"),
        "ip": s(lb, "/ip"),
        "status": s(lb, "/status"),
        "region": s(lb, "/region/slug"),
        "sizeUnit": n(lb, "/size_unit"),
        "forwardingRules": rules,
        "healthCheck": hc,
        "dropletIds": arr(lb, "/droplet_ids"),
        "tag": s(lb, "/tag"),
        "vpcUuid": s(lb, "/vpc_uuid"),
        "redirectHttpToHttps": lb.get("redirect_http_to_https").and_then(Value::as_bool).unwrap_or(false),
        "createdAt": s(lb, "/created_at"),
    })
}

/// A droplet snapshot.
#[must_use]
pub fn snapshot_json(sn: &Value) -> Value {
    json!({
        "id": sn.get("id").cloned().unwrap_or(Value::Null),
        "name": s(sn, "/name"),
        "sizeGb": sn.get("size_gigabytes").cloned().filter(Value::is_number).unwrap_or(json!(0)),
        "createdAt": s(sn, "/created_at"),
        "regions": arr(sn, "/regions"),
    })
}

/// The `/cloud/account` shape (`balance` is `null` when the token cannot read
/// billing).
#[must_use]
pub fn account_json(a: &Value, balance: Option<&Value>) -> Value {
    json!({
        "uuid": s(a, "/uuid"),
        "email": s(a, "/email"),
        "teamName": s(a, "/team/name"),
        "dropletLimit": n(a, "/droplet_limit"),
        "volumeLimit": n(a, "/volume_limit"),
        "status": s(a, "/status"),
        "balance": balance.map_or(Value::Null, |b| json!({
            "monthToDateUsage": s(b, "/month_to_date_usage"),
            "accountBalance": s(b, "/account_balance"),
            "monthToDateBalance": s(b, "/month_to_date_balance"),
            "generatedAt": s(b, "/generated_at"),
        })),
    })
}

#[must_use]
pub fn region_json(r: &Value) -> Value {
    json!({
        "slug": s(r, "/slug"),
        "name": s(r, "/name"),
        "available": r.get("available").and_then(Value::as_bool).unwrap_or(false),
        "features": arr(r, "/features"),
    })
}

#[must_use]
pub fn size_json(z: &Value) -> Value {
    let disk = n(z, "/disk");
    json!({
        "slug": s(z, "/slug"),
        "description": s(z, "/description"),
        "memoryMb": n(z, "/memory"),
        "vcpus": n(z, "/vcpus"),
        "diskGb": disk,
        "transferTb": n(z, "/transfer"),
        "priceMonthly": n(z, "/price_monthly"),
        "priceHourly": n(z, "/price_hourly"),
        "regions": arr(z, "/regions"),
        "available": z.get("available").and_then(Value::as_bool).unwrap_or(false),
    })
}

#[must_use]
pub fn image_json(i: &Value) -> Value {
    json!({
        "id": n(i, "/id"),
        "slug": s(i, "/slug"),
        "name": s(i, "/name"),
        "distribution": s(i, "/distribution"),
        "description": s(i, "/description"),
    })
}

/// Cloud-init `user_data` that installs and enrolls the agent, followed by
/// the caller's own `user_data` (as a second MIME part, so a `#cloud-config`
/// document keeps working).
#[must_use]
pub fn enroll_user_data(public_url: &str, enrollment_token: &str, caller: Option<&str>) -> String {
    let base = public_url.trim_end_matches('/');
    let script = format!(
        "#!/bin/sh\n# Linexus agent enrollment (added by Nexus)\nset -e\n\
         curl -fsSL {base}/install/agent.sh | NEXUS_URL={base} ENROLLMENT_TOKEN={enrollment_token} sh\n"
    );
    let Some(extra) = caller.map(str::trim).filter(|s| !s.is_empty()) else {
        return script;
    };
    let boundary = "==LINEXUS-NEXUS-BOUNDARY==";
    let (content_type, body) = if extra.starts_with("#cloud-config") {
        ("text/cloud-config", extra.to_string())
    } else if extra.starts_with("#!") {
        ("text/x-shellscript", extra.to_string())
    } else if extra.starts_with("#include") {
        ("text/x-include-url", extra.to_string())
    } else {
        ("text/x-shellscript", format!("#!/bin/sh\n{extra}"))
    };
    format!(
        "Content-Type: multipart/mixed; boundary=\"{boundary}\"\nMIME-Version: 1.0\n\n\
         --{boundary}\nContent-Type: text/x-shellscript; charset=\"us-ascii\"\nMIME-Version: 1.0\n\
         Content-Disposition: attachment; filename=\"10-linexus-agent.sh\"\n\n{script}\n\
         --{boundary}\nContent-Type: {content_type}; charset=\"us-ascii\"\nMIME-Version: 1.0\n\
         Content-Disposition: attachment; filename=\"20-user-data\"\n\n{body}\n\
         --{boundary}--\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn droplet_projection() {
        let d = json!({
            "id": 42, "name": "web-1", "status": "active", "memory": 2048, "vcpus": 2, "disk": 50,
            "region": {"slug": "fra1"}, "size_slug": "s-2vcpu-2gb",
            "size": {"price_monthly": 18.0},
            "image": {"slug": "ubuntu-24-04-x64"},
            "networks": {"v4": [
                {"ip_address": "10.10.0.5", "type": "private"},
                {"ip_address": "203.0.113.7", "type": "public"}
            ], "v6": []},
            "tags": ["lx-hostgroup-acme"], "volume_ids": [], "features": ["monitoring"],
            "vpc_uuid": "v", "created_at": "2026-10-09T00:00:00Z"
        });
        let p = droplet_json(&d, &[]);
        assert_eq!(p["publicIpv4"], "203.0.113.7");
        assert_eq!(p["privateIpv4"], "10.10.0.5");
        assert_eq!(p["size"], "s-2vcpu-2gb");
        assert_eq!(p["priceMonthly"], 18.0);
        assert_eq!(p["agentId"], "");
    }

    #[test]
    fn user_data_composition() {
        let plain = enroll_user_data("https://nexus.example/", "nxe_abc", None);
        assert!(plain.contains(
            "curl -fsSL https://nexus.example/install/agent.sh | NEXUS_URL=https://nexus.example ENROLLMENT_TOKEN=nxe_abc sh"
        ));
        let mixed = enroll_user_data(
            "https://n",
            "nxe_abc",
            Some("#cloud-config\npackages: [htop]"),
        );
        assert!(mixed.starts_with("Content-Type: multipart/mixed"));
        assert!(mixed.contains("text/cloud-config"));
        assert!(mixed.find("ENROLLMENT_TOKEN").unwrap() < mixed.find("packages").unwrap());
    }

    #[test]
    fn hostgroup_tag_is_sanitized() {
        assert_eq!(hostgroup_tag("acme corp"), "lx-hostgroup-acme-corp");
    }
}
