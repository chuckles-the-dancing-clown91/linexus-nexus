//! One DNS API across Cloudflare and BIND (`docs/PROVIDERS.md` §5).
//!
//! Zone ids carry their provider: `cf:<cloudflare zone id>` or
//! `bind:<uuid>`. [`provider_for`] picks the [`DnsProvider`] by that prefix;
//! record validation and the `ensure` logic are shared.

use std::net::{Ipv4Addr, Ipv6Addr};

use async_trait::async_trait;
use loco_rs::app::AppContext;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::cloudflare::Cloudflare;
use super::{bind::BindDns, ProviderError, ProviderResult, BIND, CLOUDFLARE};

/// Record types the API accepts.
pub const TYPES: [&str; 7] = ["A", "AAAA", "CNAME", "TXT", "MX", "NS", "CAA"];
/// Most characters of record content.
pub const MAX_CONTENT: usize = 2048;
pub const MIN_TTL: i64 = 60;
pub const MAX_TTL: i64 = 86_400;
/// Cloudflare's "automatic" TTL.
pub const AUTO_TTL: i64 = 1;

pub const CF_PREFIX: &str = "cf:";
pub const BIND_PREFIX: &str = "bind:";

/// The `Record` shape. `name` is the FQDN (no trailing dot).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub id: String,
    #[serde(rename = "type")]
    pub record_type: String,
    pub name: String,
    pub content: String,
    pub ttl: i64,
    pub proxied: bool,
    pub priority: Option<i64>,
    pub comment: String,
}

/// The `Zone` shape (with `records` on the detail).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Zone {
    pub id: String,
    pub provider: String,
    pub name: String,
    pub status: String,
    pub name_servers: Vec<String>,
    pub original_name_servers: Vec<String>,
    pub primary_agent_id: String,
    pub secondary_agent_ids: Vec<String>,
    pub serial: i64,
    pub apply_status: String,
    pub last_task_id: String,
    pub created_at: String,
    /// How many records the zone holds: counted for BIND, `null` for
    /// Cloudflare (its zone list carries no count).
    pub record_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub records: Option<Vec<Record>>,
    /// The TTL a record gets when none is given.
    #[serde(skip)]
    pub default_ttl: i64,
}

/// `POST /dns/zones`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateZone {
    pub provider: String,
    pub name: String,
    #[serde(default)]
    pub primary_agent_id: Option<String>,
    #[serde(default)]
    pub secondary_agent_ids: Option<Vec<String>>,
    #[serde(default)]
    pub default_ttl: Option<i64>,
    #[serde(default)]
    pub admin_email: Option<String>,
}

/// A record body: every field optional (create and ensure require `type`,
/// `name` and `content`; a PATCH merges what it carries).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordInput {
    #[serde(rename = "type", default)]
    pub record_type: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub ttl: Option<i64>,
    #[serde(default)]
    pub proxied: Option<bool>,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub comment: Option<String>,
}

impl RecordInput {
    /// `self` laid over an existing record.
    #[must_use]
    pub fn over(&self, r: &Record) -> Self {
        Self {
            record_type: self
                .record_type
                .clone()
                .or_else(|| Some(r.record_type.clone())),
            name: self.name.clone().or_else(|| Some(r.name.clone())),
            content: self.content.clone().or_else(|| Some(r.content.clone())),
            ttl: self.ttl.or(Some(r.ttl)),
            proxied: self.proxied.or(Some(r.proxied)),
            priority: self.priority.or(r.priority),
            comment: self.comment.clone().or_else(|| Some(r.comment.clone())),
        }
    }
}

/// A validated, normalized record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordSpec {
    pub record_type: String,
    /// FQDN, lowercase, no trailing dot.
    pub name: String,
    /// Normalized: hostnames lowercase without the trailing dot, TXT without
    /// surrounding quotes, CAA as `flags tag "value"`.
    pub content: String,
    pub ttl: i64,
    pub proxied: bool,
    pub priority: Option<i64>,
    pub comment: Option<String>,
}

/// The outcome of a record change: the record and, for BIND, the
/// `dns_zone_apply` task (the primary's) that carries it to the servers.
#[derive(Debug, Clone)]
pub struct RecordChange {
    pub record: Record,
    pub task_id: Option<String>,
}

fn invalid(msg: impl Into<String>) -> ProviderError {
    ProviderError::Invalid(msg.into())
}

fn valid_label(l: &str) -> bool {
    !l.is_empty()
        && l.len() <= 63
        && l.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// A hostname: lowercase labels of `[a-z0-9-_]`, at most 253 characters.
#[must_use]
pub fn valid_hostname(h: &str) -> bool {
    !h.is_empty() && h.len() <= 253 && h.split('.').all(valid_label)
}

/// A zone name: a hostname with at least two labels, letters in the TLD.
#[must_use]
pub fn valid_zone_name(name: &str) -> bool {
    valid_hostname(name)
        && name.contains('.')
        && name
            .rsplit('.')
            .next()
            .is_some_and(|tld| tld.chars().any(|c| c.is_ascii_lowercase()))
}

/// Lowercase, trimmed, without a trailing dot.
#[must_use]
pub fn normalize_zone_name(name: &str) -> String {
    name.trim().trim_end_matches('.').to_lowercase()
}

/// Resolve a record name (relative, `@`, or FQDN) to the FQDN within `zone`.
pub fn normalize_name(name: &str, zone: &str) -> ProviderResult<String> {
    let n = name.trim().trim_end_matches('.').to_lowercase();
    let fqdn = if n.is_empty() || n == "@" {
        zone.to_string()
    } else if n == zone || n.ends_with(&format!(".{zone}")) {
        n
    } else {
        format!("{n}.{zone}")
    };
    let relative = fqdn
        .strip_suffix(zone)
        .unwrap_or_default()
        .trim_end_matches('.');
    let labels_ok = relative.is_empty()
        || relative
            .split('.')
            .enumerate()
            .all(|(i, l)| (i == 0 && l == "*") || valid_label(l));
    if fqdn.len() > 253 || !labels_ok {
        return Err(invalid(format!("name: `{name}` is not a valid DNS name")));
    }
    Ok(fqdn)
}

fn normalize_host_content(c: &str, field: &str) -> ProviderResult<String> {
    let h = c.trim().trim_end_matches('.').to_lowercase();
    if valid_hostname(&h) {
        Ok(h)
    } else {
        Err(invalid(format!(
            "content: `{c}` is not a valid hostname for {field}"
        )))
    }
}

fn no_control(s: &str, field: &str) -> ProviderResult<()> {
    if s.chars().any(char::is_control) {
        return Err(invalid(format!(
            "{field}: control characters are not allowed"
        )));
    }
    Ok(())
}

/// Strip one pair of surrounding quotes from TXT content (when they are the
/// only quotes in it).
fn unquote_txt(c: &str) -> String {
    if c.len() >= 2 && c.starts_with('"') && c.ends_with('"') && !c[1..c.len() - 1].contains('"') {
        c[1..c.len() - 1].to_string()
    } else {
        c.to_string()
    }
}

/// Parse CAA content `flags tag value` → `(flags, tag, value)`.
pub fn parse_caa(c: &str) -> ProviderResult<(u8, String, String)> {
    let mut parts = c.trim().splitn(3, char::is_whitespace);
    let flags = parts
        .next()
        .and_then(|f| f.parse::<u8>().ok())
        .ok_or_else(|| invalid("content: CAA is `flags tag \"value\"` (flags 0–255)"))?;
    let tag = parts.next().unwrap_or_default().to_lowercase();
    if tag.is_empty() || tag.len() > 15 || !tag.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(invalid(
            "content: CAA tag must be alphanumeric (issue, issuewild, iodef)",
        ));
    }
    let raw = parts.next().unwrap_or_default().trim();
    let value = raw
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(raw)
        .to_string();
    if value.contains('"') || value.contains('\\') {
        return Err(invalid(
            "content: CAA value cannot contain quotes or backslashes",
        ));
    }
    Ok((flags, tag, value))
}

/// Validate and normalize a record for `zone` (`provider` decides the TTL
/// rules: `1` = automatic is Cloudflare-only).
pub fn validate(input: &RecordInput, zone: &Zone) -> ProviderResult<RecordSpec> {
    let is_cf = zone.provider == CLOUDFLARE;
    let record_type = input
        .record_type
        .as_deref()
        .map(|t| t.trim().to_uppercase())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| invalid("type: required"))?;
    if !TYPES.contains(&record_type.as_str()) {
        return Err(invalid(format!(
            "type: must be one of {}",
            TYPES.join(", ")
        )));
    }
    let name = normalize_name(
        input
            .name
            .as_deref()
            .ok_or_else(|| invalid("name: required"))?,
        &zone.name,
    )?;
    let raw = input
        .content
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| invalid("content: required"))?;
    if raw.chars().count() > MAX_CONTENT {
        return Err(invalid(format!(
            "content: at most {MAX_CONTENT} characters"
        )));
    }
    no_control(raw, "content")?;

    let content = match record_type.as_str() {
        "A" => raw
            .parse::<Ipv4Addr>()
            .map_err(|_| invalid("content: an A record needs an IPv4 address"))?
            .to_string(),
        "AAAA" => raw
            .parse::<Ipv6Addr>()
            .map_err(|_| invalid("content: an AAAA record needs an IPv6 address"))?
            .to_string(),
        "CNAME" => {
            if name == zone.name {
                return Err(invalid("name: a CNAME cannot sit at the zone apex"));
            }
            normalize_host_content(raw, "CNAME")?
        }
        "MX" => normalize_host_content(raw, "MX")?,
        "NS" => normalize_host_content(raw, "NS")?,
        "TXT" => unquote_txt(raw),
        "CAA" => {
            let (flags, tag, value) = parse_caa(raw)?;
            format!("{flags} {tag} \"{value}\"")
        }
        _ => unreachable!("type checked above"),
    };

    let ttl = input.ttl.unwrap_or(zone.default_ttl);
    if !(ttl == AUTO_TTL && is_cf) && !(MIN_TTL..=MAX_TTL).contains(&ttl) {
        return Err(invalid(if is_cf {
            "ttl: 60…86400, or 1 for automatic"
        } else {
            "ttl: 60…86400"
        }));
    }

    let priority = if record_type == "MX" {
        let p = input
            .priority
            .ok_or_else(|| invalid("priority: required for MX"))?;
        if !(0..=65_535).contains(&p) {
            return Err(invalid("priority: 0…65535"));
        }
        Some(p)
    } else {
        None
    };

    let proxied = input.proxied.unwrap_or(false);
    if proxied && !(is_cf && matches!(record_type.as_str(), "A" | "AAAA" | "CNAME")) {
        return Err(invalid(
            "proxied: only for Cloudflare A, AAAA and CNAME records",
        ));
    }

    let comment = match input.comment.as_deref().map(str::trim) {
        Some(c) if !c.is_empty() => {
            no_control(c, "comment")?;
            if c.chars().count() > 500 {
                return Err(invalid("comment: at most 500 characters"));
            }
            Some(c.to_string())
        }
        _ => None,
    };

    Ok(RecordSpec {
        record_type,
        name,
        content,
        ttl,
        proxied,
        priority,
        comment,
    })
}

/// Whether two contents of `record_type` say the same thing.
#[must_use]
pub fn same_content(record_type: &str, a: &str, b: &str) -> bool {
    match record_type {
        "A" => {
            a.parse::<Ipv4Addr>().ok() == b.parse::<Ipv4Addr>().ok()
                && a.parse::<Ipv4Addr>().is_ok()
        }
        "AAAA" => {
            a.parse::<Ipv6Addr>().ok() == b.parse::<Ipv6Addr>().ok()
                && a.parse::<Ipv6Addr>().is_ok()
        }
        "CNAME" | "MX" | "NS" => a
            .trim_end_matches('.')
            .eq_ignore_ascii_case(b.trim_end_matches('.')),
        "TXT" => unquote_txt(a) == unquote_txt(b),
        "CAA" => match (parse_caa(a), parse_caa(b)) {
            (Ok(x), Ok(y)) => x == y,
            _ => a == b,
        },
        _ => a == b,
    }
}

/// The DNS provider behind a zone id.
#[async_trait]
pub trait DnsProvider: Send + Sync {
    /// Every zone this provider holds.
    async fn list_zones(&self) -> ProviderResult<Vec<Zone>>;
    async fn create_zone(&self, req: &CreateZone) -> ProviderResult<Zone>;
    /// One zone (`zone` is the id without its prefix).
    async fn get_zone(&self, zone: &str, with_records: bool) -> ProviderResult<Zone>;
    /// Delete; returns the task that removes it from the servers (BIND).
    async fn delete_zone(&self, zone: &str) -> ProviderResult<Option<String>>;
    /// Records, optionally filtered by type and (FQDN) name.
    async fn list_records(
        &self,
        zone: &str,
        record_type: Option<&str>,
        name: Option<&str>,
    ) -> ProviderResult<Vec<Record>>;
    async fn create_record(&self, zone: &str, input: &RecordInput) -> ProviderResult<RecordChange>;
    async fn update_record(
        &self,
        zone: &str,
        record: &str,
        input: &RecordInput,
    ) -> ProviderResult<RecordChange>;
    async fn delete_record(&self, zone: &str, record: &str) -> ProviderResult<Option<String>>;
}

/// The provider for `zone_id` and the id without its prefix. An unknown
/// prefix is `NotFound`; a Cloudflare zone without credentials is
/// `NotConfigured`.
pub async fn provider_for(
    ctx: &AppContext,
    zone_id: &str,
    requester: &str,
) -> ProviderResult<(Box<dyn DnsProvider>, String)> {
    if let Some(id) = zone_id.strip_prefix(CF_PREFIX) {
        let cf = Cloudflare::from_ctx(ctx).await?;
        return Ok((Box::new(CloudflareDns { cf }), id.to_string()));
    }
    if let Some(id) = zone_id.strip_prefix(BIND_PREFIX) {
        return Ok((
            Box::new(BindDns::new(ctx.clone(), requester.to_string())),
            id.to_string(),
        ));
    }
    Err(ProviderError::NotFound(format!("no such zone: {zone_id}")))
}

/// The provider named in a create request.
pub async fn provider_named(
    ctx: &AppContext,
    name: &str,
    requester: &str,
) -> ProviderResult<Box<dyn DnsProvider>> {
    match name {
        CLOUDFLARE => Ok(Box::new(CloudflareDns {
            cf: Cloudflare::from_ctx(ctx).await?,
        })),
        BIND => Ok(Box::new(BindDns::new(ctx.clone(), requester.to_string()))),
        _ => Err(invalid("provider: must be cloudflare or bind")),
    }
}

/// `POST …/records/ensure`: find by type + name; update when different,
/// create when absent. Several records of that type and name and none with
/// this content is a `Conflict` (the caller must say which one to change).
pub async fn ensure(
    p: &dyn DnsProvider,
    zone_id: &str,
    input: &RecordInput,
) -> ProviderResult<(RecordChange, bool)> {
    let zone = p.get_zone(zone_id, false).await?;
    let spec = validate(input, &zone)?;
    let existing = p
        .list_records(zone_id, Some(&spec.record_type), Some(&spec.name))
        .await?;
    let differs = |r: &Record| {
        input.ttl.is_some_and(|t| t != r.ttl)
            || input.proxied.is_some_and(|x| x != r.proxied)
            || (spec.record_type == "MX" && spec.priority != r.priority)
    };
    if let Some(same) = existing
        .iter()
        .find(|r| same_content(&spec.record_type, &r.content, &spec.content))
    {
        if !differs(same) {
            return Ok((
                RecordChange {
                    record: same.clone(),
                    task_id: None,
                },
                false,
            ));
        }
        let change = p.update_record(zone_id, &same.id, input).await?;
        return Ok((change, true));
    }
    match existing.as_slice() {
        [] => Ok((p.create_record(zone_id, input).await?, true)),
        [one] => Ok((p.update_record(zone_id, &one.id, input).await?, true)),
        many => Err(ProviderError::Conflict(format!(
            "{} {} records exist at {}; update one by id",
            many.len(),
            spec.record_type,
            spec.name
        ))),
    }
}

// ---------------------------------------------------------------------------
// Cloudflare
// ---------------------------------------------------------------------------

/// Cloudflare as a [`DnsProvider`].
pub struct CloudflareDns {
    pub cf: Cloudflare,
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// A Cloudflare zone as a `Zone`.
#[must_use]
pub fn cf_zone(z: &Value) -> Zone {
    let s = |k: &str| {
        z.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    Zone {
        id: format!("{CF_PREFIX}{}", s("id")),
        provider: CLOUDFLARE.to_string(),
        name: s("name"),
        status: s("status"),
        name_servers: str_list(z.get("name_servers")),
        original_name_servers: str_list(z.get("original_name_servers")),
        primary_agent_id: String::new(),
        secondary_agent_ids: Vec::new(),
        serial: 0,
        apply_status: String::new(),
        last_task_id: String::new(),
        created_at: s("created_on"),
        record_count: None,
        records: None,
        default_ttl: AUTO_TTL,
    }
}

/// A Cloudflare DNS record as a `Record`.
#[must_use]
pub fn cf_record(r: &Value) -> Record {
    let s = |k: &str| {
        r.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let record_type = s("type");
    let mut content = s("content");
    if record_type == "TXT" {
        content = unquote_txt(&content);
    }
    if record_type == "CAA" {
        if let Some(d) = r.get("data").filter(|d| d.is_object()) {
            let flags = d.get("flags").and_then(Value::as_u64).unwrap_or(0);
            let tag = d.get("tag").and_then(Value::as_str).unwrap_or_default();
            let value = d.get("value").and_then(Value::as_str).unwrap_or_default();
            content = format!("{flags} {tag} \"{value}\"");
        }
    }
    Record {
        id: s("id"),
        record_type,
        name: s("name"),
        content,
        ttl: r.get("ttl").and_then(Value::as_i64).unwrap_or(AUTO_TTL),
        proxied: r.get("proxied").and_then(Value::as_bool).unwrap_or(false),
        priority: r.get("priority").and_then(Value::as_i64),
        comment: s("comment"),
    }
}

/// The Cloudflare request body for a validated record.
#[must_use]
pub fn cf_record_body(spec: &RecordSpec) -> Value {
    let mut body = json!({
        "type": spec.record_type,
        "name": spec.name,
        "ttl": spec.ttl,
        "comment": spec.comment.clone().unwrap_or_default(),
    });
    if spec.record_type == "CAA" {
        if let Ok((flags, tag, value)) = parse_caa(&spec.content) {
            body["data"] = json!({"flags": flags, "tag": tag, "value": value});
        }
    } else {
        body["content"] = json!(spec.content);
    }
    if matches!(spec.record_type.as_str(), "A" | "AAAA" | "CNAME") {
        body["proxied"] = json!(spec.proxied);
    }
    if let Some(p) = spec.priority {
        body["priority"] = json!(p);
    }
    body
}

#[async_trait]
impl DnsProvider for CloudflareDns {
    async fn list_zones(&self) -> ProviderResult<Vec<Zone>> {
        Ok(self.cf.zones().await?.iter().map(cf_zone).collect())
    }

    async fn create_zone(&self, req: &CreateZone) -> ProviderResult<Zone> {
        let name = normalize_zone_name(&req.name);
        if !valid_zone_name(&name) {
            return Err(invalid("name: not a valid zone name"));
        }
        Ok(cf_zone(&self.cf.create_zone(&name).await?))
    }

    async fn get_zone(&self, zone: &str, with_records: bool) -> ProviderResult<Zone> {
        let mut z = cf_zone(&self.cf.zone(zone).await?);
        if with_records {
            z.records = Some(self.list_records(zone, None, None).await?);
        }
        Ok(z)
    }

    async fn delete_zone(&self, zone: &str) -> ProviderResult<Option<String>> {
        self.cf.delete_zone(zone).await?;
        Ok(None)
    }

    async fn list_records(
        &self,
        zone: &str,
        record_type: Option<&str>,
        name: Option<&str>,
    ) -> ProviderResult<Vec<Record>> {
        Ok(self
            .cf
            .records(zone, record_type, name)
            .await?
            .iter()
            .map(cf_record)
            .collect())
    }

    async fn create_record(&self, zone: &str, input: &RecordInput) -> ProviderResult<RecordChange> {
        let z = cf_zone(&self.cf.zone(zone).await?);
        let spec = validate(input, &z)?;
        let r = self.cf.create_record(zone, &cf_record_body(&spec)).await?;
        Ok(RecordChange {
            record: cf_record(&r),
            task_id: None,
        })
    }

    async fn update_record(
        &self,
        zone: &str,
        record: &str,
        input: &RecordInput,
    ) -> ProviderResult<RecordChange> {
        let z = cf_zone(&self.cf.zone(zone).await?);
        let current = cf_record(&self.cf.record(zone, record).await?);
        let spec = validate(&input.over(&current), &z)?;
        let r = self
            .cf
            .update_record(zone, record, &cf_record_body(&spec))
            .await?;
        Ok(RecordChange {
            record: cf_record(&r),
            task_id: None,
        })
    }

    async fn delete_record(&self, zone: &str, record: &str) -> ProviderResult<Option<String>> {
        self.cf.delete_record(zone, record).await?;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zone(provider: &str) -> Zone {
        Zone {
            id: String::new(),
            provider: provider.to_string(),
            name: "example.com".into(),
            status: String::new(),
            name_servers: vec![],
            original_name_servers: vec![],
            primary_agent_id: String::new(),
            secondary_agent_ids: vec![],
            serial: 0,
            apply_status: String::new(),
            last_task_id: String::new(),
            created_at: String::new(),
            record_count: None,
            records: None,
            default_ttl: if provider == CLOUDFLARE { 1 } else { 3600 },
        }
    }

    fn input(t: &str, n: &str, c: &str) -> RecordInput {
        RecordInput {
            record_type: Some(t.into()),
            name: Some(n.into()),
            content: Some(c.into()),
            ..Default::default()
        }
    }

    #[test]
    fn names_normalize_to_fqdn() {
        assert_eq!(normalize_name("@", "example.com").unwrap(), "example.com");
        assert_eq!(
            normalize_name("www", "example.com").unwrap(),
            "www.example.com"
        );
        assert_eq!(
            normalize_name("WWW.Example.com.", "example.com").unwrap(),
            "www.example.com"
        );
        assert_eq!(
            normalize_name("*.app", "example.com").unwrap(),
            "*.app.example.com"
        );
        assert!(normalize_name("bad name", "example.com").is_err());
        assert!(normalize_name("a\nb", "example.com").is_err());
    }

    #[test]
    fn validation_rules() {
        let cf = zone(CLOUDFLARE);
        let bind = zone(BIND);
        assert!(validate(&input("A", "www", "1.2.3.4"), &cf).is_ok());
        assert!(validate(&input("A", "www", "::1"), &cf).is_err());
        assert!(validate(&input("AAAA", "www", "2001:db8::1"), &cf).is_ok());
        assert!(validate(&input("CNAME", "@", "target.example.net"), &cf).is_err());
        assert!(validate(&input("MX", "@", "mail.example.com"), &cf).is_err());
        let mut mx = input("MX", "@", "mail.example.com.");
        mx.priority = Some(10);
        assert_eq!(validate(&mx, &cf).unwrap().content, "mail.example.com");
        assert_eq!(validate(&input("A", "x", "1.2.3.4"), &cf).unwrap().ttl, 1);
        assert_eq!(
            validate(&input("A", "x", "1.2.3.4"), &bind).unwrap().ttl,
            3600
        );
        let mut auto = input("A", "x", "1.2.3.4");
        auto.ttl = Some(1);
        assert!(validate(&auto, &bind).is_err());
        assert!(validate(&input("TXT", "x", &"a".repeat(2049)), &cf).is_err());
        assert!(validate(&input("TXT", "x", "line\nbreak"), &bind).is_err());
        assert!(validate(&input("SRV", "x", "y"), &cf).is_err());
        assert_eq!(
            validate(&input("CAA", "@", "0 issue letsencrypt.org"), &bind)
                .unwrap()
                .content,
            "0 issue \"letsencrypt.org\""
        );
        let mut proxied_txt = input("TXT", "x", "v");
        proxied_txt.proxied = Some(true);
        assert!(validate(&proxied_txt, &cf).is_err());
    }

    #[test]
    fn content_comparison() {
        assert!(same_content("AAAA", "2001:db8:0::1", "2001:db8::1"));
        assert!(same_content("CNAME", "a.example.com.", "A.example.com"));
        assert!(same_content("TXT", "\"v=spf1 -all\"", "v=spf1 -all"));
        assert!(!same_content("A", "1.2.3.4", "1.2.3.5"));
    }
}
