//! BIND zones served by our own agents.
//!
//! Nexus's database is the source of truth. Every change bumps the SOA serial
//! (`YYYYMMDDnn`), renders the whole zone file and dispatches a
//! `dns_zone_apply` task — through the Orchestrator, like any other task — to
//! the primary (`role=primary`, `secondaries=<their IPs>`) and to each
//! secondary (`role=secondary`, `primaries=<primary IP>`). Applies for an older serial that no agent has
//! picked up yet are cancelled: each apply carries the whole zone, so only
//! the newest matters, and a stale one must never land after it.
//! `applyStatus` is derived from the latest change's tasks.

use std::collections::BTreeMap;

use async_trait::async_trait;
use loco_rs::app::AppContext;
use loco_rs::model::ModelError;
use sea_orm::{ActiveModelTrait, ActiveValue};
use uuid::Uuid;

use super::dns::{
    normalize_name, normalize_zone_name, valid_hostname, valid_zone_name, validate, CreateZone,
    DnsProvider, Record, RecordChange, RecordInput, RecordSpec, Zone, BIND_PREFIX, MAX_TTL,
    MIN_TTL,
};
use super::{ProviderError, ProviderResult, BIND};
use crate::dispatch::{self, DispatchRequest};
use crate::models::{agents, dns_zones, tasks};

pub const INTENT_APPLY: &str = "dns_zone_apply";
pub const INTENT_REMOVE: &str = "dns_zone_remove";
pub const DEFAULT_TTL: i64 = 3600;

const SOA_REFRESH: u32 = 3600;
const SOA_RETRY: u32 = 900;
const SOA_EXPIRE: u32 = 1_209_600;
const SOA_MINIMUM: u32 = 300;

fn db_err(e: impl std::fmt::Display) -> ProviderError {
    ProviderError::Unreachable(format!("database: {e}"))
}

fn model_err(e: ModelError) -> ProviderError {
    match e {
        ModelError::EntityNotFound => ProviderError::NotFound("not found".into()),
        other => db_err(other),
    }
}

// ---------------------------------------------------------------------------
// Rendering (pure)
// ---------------------------------------------------------------------------

/// One name server of a zone, with its glue address when it sits inside the
/// zone and its agent's public IP is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameServer {
    pub fqdn: String,
    pub glue: Option<String>,
}

/// The name servers for `zone`: the primary first, then each secondary. An
/// agent whose hostname is a FQDN serves under that name; otherwise it is
/// `ns<n>.<zone>` (n = its position, from 1), glued to its public IP.
#[must_use]
pub fn name_servers(zone: &str, servers: &[&agents::Model]) -> Vec<NameServer> {
    let mut out: Vec<NameServer> = Vec::new();
    for (i, a) in servers.iter().enumerate() {
        let host = a.hostname.trim().trim_end_matches('.').to_lowercase();
        let fqdn = if host.contains('.') && valid_hostname(&host) {
            host
        } else {
            format!("ns{}.{zone}", i + 1)
        };
        if out.iter().any(|n| n.fqdn == fqdn) {
            continue;
        }
        let in_zone = fqdn == zone || fqdn.ends_with(&format!(".{zone}"));
        let glue = a
            .public_ip
            .clone()
            .filter(|ip| in_zone && ip.parse::<std::net::IpAddr>().is_ok());
        out.push(NameServer { fqdn, glue });
    }
    out
}

/// The SOA RNAME for an email: dots in the local part escaped, `@` → `.`.
#[must_use]
pub fn rname(email: &str) -> String {
    match email.split_once('@') {
        Some((local, domain)) => format!("{}.{}.", local.replace('.', "\\."), domain),
        None => format!("{email}."),
    }
}

/// Quote TXT content as one or more `"…"` strings of at most 255 bytes.
#[must_use]
pub fn quote_txt(content: &str) -> String {
    let mut chunks: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in content.chars() {
        if cur.len() + c.len_utf8() > 255 {
            chunks.push(std::mem::take(&mut cur));
        }
        cur.push(c);
    }
    if !cur.is_empty() || chunks.is_empty() {
        chunks.push(cur);
    }
    chunks
        .iter()
        .map(|c| format!("\"{}\"", c.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn absolute(name: &str) -> String {
    format!("{}.", name.trim_end_matches('.'))
}

/// The record data column for a validated record.
fn rdata(r: &dns_zones::Record) -> String {
    match r.record_type.as_str() {
        "CNAME" | "NS" => absolute(&r.content),
        "MX" => format!("{} {}", r.priority.unwrap_or(10), absolute(&r.content)),
        "TXT" => quote_txt(&r.content),
        _ => r.content.clone(),
    }
}

/// Everything a zone file is rendered from.
pub struct ZoneFile<'a> {
    pub zone: &'a str,
    pub serial: i64,
    pub default_ttl: i64,
    pub admin_email: &'a str,
    pub name_servers: &'a [NameServer],
    pub records: &'a [dns_zones::Record],
}

/// Render the zone file: `$ORIGIN`, `$TTL`, SOA, NS (+ glue), then records.
/// Every name is written absolute; contents were validated on the way in, so
/// nothing a caller sent can break out of its field.
#[must_use]
pub fn render(z: &ZoneFile<'_>) -> String {
    let origin = absolute(z.zone);
    let primary_ns = z
        .name_servers
        .first()
        .map_or_else(|| format!("ns1.{origin}"), |n| absolute(&n.fqdn));
    let mut out = String::new();
    out.push_str(&format!(
        "; {zone} -- rendered by Linexus Nexus; edits here are overwritten\n\
         $ORIGIN {origin}\n$TTL {ttl}\n\
         @\tIN\tSOA\t{primary_ns} {rname} (\n\
         \t\t{serial}\t; serial\n\
         \t\t{SOA_REFRESH}\t\t; refresh\n\
         \t\t{SOA_RETRY}\t\t; retry\n\
         \t\t{SOA_EXPIRE}\t\t; expire\n\
         \t\t{SOA_MINIMUM} )\t\t; negative caching TTL\n",
        zone = z.zone,
        ttl = z.default_ttl,
        rname = rname(z.admin_email),
        serial = z.serial,
    ));
    for ns in z.name_servers {
        out.push_str(&format!(
            "@\t{}\tIN\tNS\t{}\n",
            z.default_ttl,
            absolute(&ns.fqdn)
        ));
    }
    for ns in z.name_servers {
        let Some(ip) = &ns.glue else { continue };
        let rtype = if ip.contains(':') { "AAAA" } else { "A" };
        let shadowed = z
            .records
            .iter()
            .any(|r| r.name == ns.fqdn && r.record_type == rtype);
        if !shadowed {
            out.push_str(&format!(
                "{}\t{}\tIN\t{rtype}\t{ip}\n",
                absolute(&ns.fqdn),
                z.default_ttl
            ));
        }
    }
    for r in z.records {
        out.push_str(&format!(
            "{}\t{}\tIN\t{}\t{}\n",
            absolute(&r.name),
            r.ttl,
            r.record_type,
            rdata(r)
        ));
    }
    out
}

fn valid_email(e: &str) -> bool {
    let Some((local, domain)) = e.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && local.len() <= 64
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
        && !local.starts_with('.')
        && !local.ends_with('.')
        && valid_hostname(&domain.to_lowercase())
}

// ---------------------------------------------------------------------------
// The provider
// ---------------------------------------------------------------------------

/// BIND (our agents) as a [`super::dns::DnsProvider`].
pub struct BindDns {
    ctx: AppContext,
    requester: String,
}

fn record_json(r: &dns_zones::Record) -> Record {
    Record {
        id: r.record_id.to_string(),
        record_type: r.record_type.clone(),
        name: r.name.clone(),
        content: r.content.clone(),
        ttl: i64::from(r.ttl),
        proxied: false,
        priority: r.priority.map(i64::from),
        comment: r.comment.clone().unwrap_or_default(),
    }
}

fn parse_zone_uuid(zone: &str) -> ProviderResult<Uuid> {
    Uuid::parse_str(zone)
        .map_err(|_| ProviderError::NotFound(format!("no such zone: {BIND_PREFIX}{zone}")))
}

fn ttl_i32(ttl: i64) -> i32 {
    i32::try_from(ttl.clamp(MIN_TTL, MAX_TTL)).unwrap_or(3600)
}

impl BindDns {
    #[must_use]
    pub const fn new(ctx: AppContext, requester: String) -> Self {
        Self { ctx, requester }
    }

    async fn zone_row(&self, zone: &str) -> ProviderResult<dns_zones::Model> {
        let id = parse_zone_uuid(zone)?;
        dns_zones::Model::find_by_zone_id(&self.ctx.db, &id)
            .await
            .map_err(|e| match e {
                ModelError::EntityNotFound => {
                    ProviderError::NotFound(format!("no such zone: {BIND_PREFIX}{zone}"))
                }
                other => db_err(other),
            })
    }

    async fn agent(&self, id: &Uuid) -> ProviderResult<Option<agents::Model>> {
        match agents::Model::find_by_agent_id(&self.ctx.db, id).await {
            Ok(a) => Ok(Some(a)),
            Err(ModelError::EntityNotFound) => Ok(None),
            Err(e) => Err(db_err(e)),
        }
    }

    /// The zone's primary and secondaries (those that still exist).
    async fn servers(
        &self,
        z: &dns_zones::Model,
    ) -> ProviderResult<(agents::Model, Vec<agents::Model>)> {
        let primary = self.agent(&z.primary_agent_id).await?.ok_or_else(|| {
            ProviderError::Invalid(format!(
                "the primary agent {} of {} no longer exists",
                z.primary_agent_id, z.name
            ))
        })?;
        let mut secondaries = Vec::new();
        for id in z.secondaries() {
            if let Some(a) = self.agent(&id).await? {
                secondaries.push(a);
            }
        }
        Ok((primary, secondaries))
    }

    /// `pending | applied | failed` from the latest change's tasks.
    async fn apply_status(&self, z: &dns_zones::Model) -> String {
        let ids = z.task_list();
        if ids.is_empty() {
            return "pending".to_string();
        }
        let mut all_done = true;
        for id in ids {
            let Ok(uuid) = Uuid::parse_str(&id) else {
                continue;
            };
            match tasks::Model::find_by_task_id(&self.ctx.db, &uuid).await {
                Ok(t) => match t.status.as_str() {
                    "completed" => {}
                    "failed" | "cancelled" => return "failed".to_string(),
                    _ => all_done = false,
                },
                Err(_) => all_done = false,
            }
        }
        if all_done { "applied" } else { "pending" }.to_string()
    }

    async fn to_zone(&self, z: &dns_zones::Model, with_records: bool) -> ProviderResult<Zone> {
        let mut servers: Vec<agents::Model> = Vec::new();
        if let Some(p) = self.agent(&z.primary_agent_id).await? {
            servers.push(p);
        }
        for id in z.secondaries() {
            if let Some(a) = self.agent(&id).await? {
                servers.push(a);
            }
        }
        let refs: Vec<&agents::Model> = servers.iter().collect();
        let ns = name_servers(&z.name, &refs);
        let (records, record_count) = if with_records {
            let records: Vec<_> = z
                .records(&self.ctx.db)
                .await
                .map_err(db_err)?
                .iter()
                .map(record_json)
                .collect();
            let count = records.len() as u64;
            (Some(records), count)
        } else {
            (None, z.record_count(&self.ctx.db).await.map_err(db_err)?)
        };
        Ok(Zone {
            id: format!("{BIND_PREFIX}{}", z.zone_id),
            provider: BIND.to_string(),
            name: z.name.clone(),
            status: "active".to_string(),
            name_servers: ns.into_iter().map(|n| n.fqdn).collect(),
            original_name_servers: Vec::new(),
            primary_agent_id: z.primary_agent_id.to_string(),
            secondary_agent_ids: z.secondaries().iter().map(ToString::to_string).collect(),
            serial: z.serial,
            apply_status: self.apply_status(z).await,
            last_task_id: z.last_task_id.clone().unwrap_or_default(),
            created_at: z.created_at.to_rfc3339(),
            record_count: Some(record_count),
            records,
            default_ttl: i64::from(z.default_ttl),
        })
    }

    /// Cancel the zone's earlier tasks that no agent has picked up yet.
    async fn supersede(&self, z: &dns_zones::Model) -> ProviderResult<()> {
        for id in z.task_list() {
            if let Ok(uuid) = Uuid::parse_str(&id) {
                tasks::Model::transition(
                    &self.ctx.db,
                    &uuid,
                    &["pending", "accepted", "planned"],
                    "cancelled",
                    None,
                )
                .await
                .map_err(db_err)?;
            }
        }
        Ok(())
    }

    async fn send(
        &self,
        intent: &str,
        agent: &agents::Model,
        params: BTreeMap<String, String>,
    ) -> ProviderResult<String> {
        let task = dispatch::dispatch(
            &self.ctx,
            &DispatchRequest {
                intent: intent.to_string(),
                targets: vec![agent.agent_id.to_string()],
                requester: self.requester.clone(),
                auto_rollback: false,
                params,
            },
        )
        .await
        .map_err(db_err)?;
        Ok(task.task_id.to_string())
    }

    /// Bump the serial, render, and dispatch `dns_zone_apply` to every
    /// server. Returns the zone and the primary's task id.
    async fn apply(&self, z: dns_zones::Model) -> ProviderResult<(dns_zones::Model, String)> {
        let z = z.bump_serial(&self.ctx.db).await.map_err(db_err)?;
        let (primary, secondaries) = self.servers(&z).await?;
        let records = z.records(&self.ctx.db).await.map_err(db_err)?;
        let mut all: Vec<&agents::Model> = vec![&primary];
        all.extend(secondaries.iter());
        let ns = name_servers(&z.name, &all);
        let content = render(&ZoneFile {
            zone: &z.name,
            serial: z.serial,
            default_ttl: i64::from(z.default_ttl),
            admin_email: &z.admin_email,
            name_servers: &ns,
            records: &records,
        });
        self.supersede(&z).await?;

        let base = |role: &str| {
            let mut p = BTreeMap::new();
            p.insert("zone".to_string(), z.name.clone());
            p.insert("content".to_string(), content.clone());
            p.insert("role".to_string(), role.to_string());
            p.insert("serial".to_string(), z.serial.to_string());
            p
        };
        // The primary needs its secondaries' addresses for allow-transfer /
        // also-notify; without them it renders `allow-transfer { none; }`.
        let secondary_ips: Vec<String> = secondaries
            .iter()
            .filter_map(agents::Model::reachable_ip)
            .collect();
        let mut primary_params = base("primary");
        primary_params.insert("secondaries".to_string(), secondary_ips.join(","));
        let mut ids = vec![self.send(INTENT_APPLY, &primary, primary_params).await?];
        let primary_ip = primary.reachable_ip().unwrap_or_default();
        for s in &secondaries {
            let mut p = base("secondary");
            p.insert("primaries".to_string(), primary_ip.clone());
            ids.push(self.send(INTENT_APPLY, s, p).await?);
        }
        let primary_task = ids[0].clone();
        let z = z.set_tasks(&self.ctx.db, &ids).await.map_err(db_err)?;
        Ok((z, primary_task))
    }

    /// Validate a record for this zone; also enforce the CNAME rule BIND
    /// would reject (a CNAME shares its name with nothing).
    async fn check(
        &self,
        z: &dns_zones::Model,
        input: &RecordInput,
        replacing: Option<&Uuid>,
    ) -> ProviderResult<RecordSpec> {
        let zone = self.to_zone(z, false).await?;
        let spec = validate(input, &zone)?;
        let existing = z.records(&self.ctx.db).await.map_err(db_err)?;
        let clash = existing.iter().any(|r| {
            Some(&r.record_id) != replacing
                && r.name == spec.name
                && (r.record_type == "CNAME") != (spec.record_type == "CNAME")
        }) || (spec.record_type == "CNAME"
            && existing
                .iter()
                .any(|r| Some(&r.record_id) != replacing && r.name == spec.name));
        if clash {
            return Err(ProviderError::Conflict(format!(
                "a CNAME cannot share the name {} with other records",
                spec.name
            )));
        }
        Ok(spec)
    }
}

#[async_trait]
impl DnsProvider for BindDns {
    async fn list_zones(&self) -> ProviderResult<Vec<Zone>> {
        let rows = dns_zones::Model::list(&self.ctx.db).await.map_err(db_err)?;
        let mut out = Vec::with_capacity(rows.len());
        for z in &rows {
            out.push(self.to_zone(z, false).await?);
        }
        Ok(out)
    }

    async fn create_zone(&self, req: &CreateZone) -> ProviderResult<Zone> {
        let name = normalize_zone_name(&req.name);
        if !valid_zone_name(&name) {
            return Err(ProviderError::Invalid("name: not a valid zone name".into()));
        }
        let primary_raw = req
            .primary_agent_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| ProviderError::Invalid("primaryAgentId: required for bind".into()))?;
        let primary_id = Uuid::parse_str(primary_raw.trim())
            .map_err(|_| ProviderError::Invalid("primaryAgentId: not an agent id".into()))?;
        if self.agent(&primary_id).await?.is_none() {
            return Err(ProviderError::NotFound(format!(
                "primaryAgentId: no such agent {primary_id}"
            )));
        }
        let mut secondaries: Vec<String> = Vec::new();
        for raw in req.secondary_agent_ids.clone().unwrap_or_default() {
            let id = Uuid::parse_str(raw.trim()).map_err(|_| {
                ProviderError::Invalid(format!("secondaryAgentIds: `{raw}` is not an agent id"))
            })?;
            if id == primary_id || secondaries.contains(&id.to_string()) {
                continue;
            }
            if self.agent(&id).await?.is_none() {
                return Err(ProviderError::NotFound(format!(
                    "secondaryAgentIds: no such agent {id}"
                )));
            }
            secondaries.push(id.to_string());
        }
        let ttl = req.default_ttl.unwrap_or(DEFAULT_TTL);
        if !(MIN_TTL..=MAX_TTL).contains(&ttl) {
            return Err(ProviderError::Invalid("defaultTtl: 60…86400".into()));
        }
        let admin_email = req
            .admin_email
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map_or_else(|| format!("hostmaster@{name}"), ToString::to_string);
        if !valid_email(&admin_email) {
            return Err(ProviderError::Invalid(
                "adminEmail: not a valid address".into(),
            ));
        }
        if dns_zones::Model::find_by_name(&self.ctx.db, &name)
            .await
            .map_err(db_err)?
            .is_some()
        {
            return Err(ProviderError::Conflict(format!(
                "zone {name} already exists"
            )));
        }
        let row = dns_zones::ActiveModel {
            zone_id: ActiveValue::set(Uuid::new_v4()),
            name: ActiveValue::set(name.clone()),
            primary_agent_id: ActiveValue::set(primary_id),
            secondary_agent_ids: ActiveValue::set(Some(
                serde_json::to_string(&secondaries).unwrap_or_else(|_| "[]".into()),
            )),
            default_ttl: ActiveValue::set(ttl_i32(ttl)),
            admin_email: ActiveValue::set(admin_email),
            serial: ActiveValue::set(0),
            ..Default::default()
        }
        .insert(&self.ctx.db)
        .await
        .map_err(|e| {
            // A concurrent create of the same name lost the unique index race.
            if e.to_string().to_lowercase().contains("unique") {
                ProviderError::Conflict(format!("zone {name} already exists"))
            } else {
                db_err(e)
            }
        })?;
        let (row, _) = self.apply(row).await?;
        self.to_zone(&row, false).await
    }

    async fn get_zone(&self, zone: &str, with_records: bool) -> ProviderResult<Zone> {
        let z = self.zone_row(zone).await?;
        self.to_zone(&z, with_records).await
    }

    async fn delete_zone(&self, zone: &str) -> ProviderResult<Option<String>> {
        let z = self.zone_row(zone).await?;
        self.supersede(&z).await?;
        let mut first = None;
        let mut servers: Vec<agents::Model> = Vec::new();
        if let Some(p) = self.agent(&z.primary_agent_id).await? {
            servers.push(p);
        }
        for id in z.secondaries() {
            if let Some(a) = self.agent(&id).await? {
                servers.push(a);
            }
        }
        for a in &servers {
            let mut p = BTreeMap::new();
            p.insert("zone".to_string(), z.name.clone());
            let id = self.send(INTENT_REMOVE, a, p).await?;
            first.get_or_insert(id);
        }
        z.delete_with_records(&self.ctx.db).await.map_err(db_err)?;
        Ok(first)
    }

    async fn list_records(
        &self,
        zone: &str,
        record_type: Option<&str>,
        name: Option<&str>,
    ) -> ProviderResult<Vec<Record>> {
        let z = self.zone_row(zone).await?;
        let name = match name {
            Some(n) => Some(normalize_name(n, &z.name)?),
            None => None,
        };
        let records = z.records(&self.ctx.db).await.map_err(db_err)?;
        Ok(records
            .iter()
            .filter(|r| record_type.is_none_or(|t| r.record_type.eq_ignore_ascii_case(t)))
            .filter(|r| name.as_ref().is_none_or(|n| &r.name == n))
            .map(record_json)
            .collect())
    }

    async fn create_record(&self, zone: &str, input: &RecordInput) -> ProviderResult<RecordChange> {
        let z = self.zone_row(zone).await?;
        let spec = self.check(&z, input, None).await?;
        let row = dns_zones::RecordActiveModel {
            record_id: ActiveValue::set(Uuid::new_v4()),
            zone_id: ActiveValue::set(z.zone_id),
            record_type: ActiveValue::set(spec.record_type.clone()),
            name: ActiveValue::set(spec.name.clone()),
            content: ActiveValue::set(spec.content.clone()),
            ttl: ActiveValue::set(ttl_i32(spec.ttl)),
            priority: ActiveValue::set(spec.priority.and_then(|p| i32::try_from(p).ok())),
            comment: ActiveValue::set(spec.comment.clone()),
            ..Default::default()
        }
        .insert(&self.ctx.db)
        .await
        .map_err(db_err)?;
        let (_, task) = self.apply(z).await?;
        Ok(RecordChange {
            record: record_json(&row),
            task_id: Some(task),
        })
    }

    async fn update_record(
        &self,
        zone: &str,
        record: &str,
        input: &RecordInput,
    ) -> ProviderResult<RecordChange> {
        let z = self.zone_row(zone).await?;
        let rid = Uuid::parse_str(record)
            .map_err(|_| ProviderError::NotFound(format!("no such record: {record}")))?;
        let current = z.find_record(&self.ctx.db, &rid).await.map_err(model_err)?;
        let merged = input.over(&record_json(&current));
        let spec = self.check(&z, &merged, Some(&rid)).await?;
        let mut active: dns_zones::RecordActiveModel = current.into();
        active.record_type = ActiveValue::set(spec.record_type.clone());
        active.name = ActiveValue::set(spec.name.clone());
        active.content = ActiveValue::set(spec.content.clone());
        active.ttl = ActiveValue::set(ttl_i32(spec.ttl));
        active.priority = ActiveValue::set(spec.priority.and_then(|p| i32::try_from(p).ok()));
        active.comment = ActiveValue::set(spec.comment.clone());
        active.updated_at = ActiveValue::set(chrono::Utc::now().into());
        let row = active.update(&self.ctx.db).await.map_err(db_err)?;
        let (_, task) = self.apply(z).await?;
        Ok(RecordChange {
            record: record_json(&row),
            task_id: Some(task),
        })
    }

    async fn delete_record(&self, zone: &str, record: &str) -> ProviderResult<Option<String>> {
        use sea_orm::ModelTrait;
        let z = self.zone_row(zone).await?;
        let rid = Uuid::parse_str(record)
            .map_err(|_| ProviderError::NotFound(format!("no such record: {record}")))?;
        let current = z.find_record(&self.ctx.db, &rid).await.map_err(model_err)?;
        current.delete(&self.ctx.db).await.map_err(db_err)?;
        let (_, task) = self.apply(z).await?;
        Ok(Some(task))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(hostname: &str, public_ip: Option<&str>) -> agents::Model {
        let now: sea_orm::prelude::DateTimeWithTimeZone = chrono::Utc::now().into();
        agents::Model {
            created_at: now,
            updated_at: now,
            id: 1,
            agent_id: Uuid::new_v4(),
            hostname: hostname.into(),
            status: "healthy".into(),
            capability_manifest: None,
            enrolled_at: None,
            last_heartbeat_at: None,
            hostgroup: None,
            os: None,
            kernel: None,
            arch: None,
            cpu_cores: None,
            memory_mb: None,
            disk_gb: None,
            agent_version: None,
            uptime_seconds: None,
            environment: "production".into(),
            monitored: true,
            monitor_note: None,
            environment_updated_at: None,
            machine_id: None,
            credential_hash: None,
            enrollment_token_id: None,
            metadata: None,
            public_ip: public_ip.map(Into::into),
            interfaces: None,
            listening: None,
            services: None,
            packages: None,
            dns_server: None,
            facts_at: None,
            dns_install_task_id: None,
        }
    }

    fn rec(t: &str, name: &str, content: &str, prio: Option<i32>) -> dns_zones::Record {
        let now: sea_orm::prelude::DateTimeWithTimeZone = chrono::Utc::now().into();
        dns_zones::Record {
            created_at: now,
            updated_at: now,
            id: 1,
            record_id: Uuid::new_v4(),
            zone_id: Uuid::new_v4(),
            record_type: t.into(),
            name: name.into(),
            content: content.into(),
            ttl: 300,
            priority: prio,
            comment: None,
        }
    }

    #[test]
    fn name_servers_and_glue() {
        let p = agent("dns-a", Some("203.0.113.7"));
        let s = agent("ns.other.net", Some("198.51.100.1"));
        let ns = name_servers("example.com", &[&p, &s]);
        assert_eq!(
            ns,
            vec![
                NameServer {
                    fqdn: "ns1.example.com".into(),
                    glue: Some("203.0.113.7".into())
                },
                NameServer {
                    fqdn: "ns.other.net".into(),
                    glue: None
                },
            ]
        );
    }

    #[test]
    fn renders_a_zone_file() {
        let ns = vec![NameServer {
            fqdn: "ns1.example.com".into(),
            glue: Some("203.0.113.7".into()),
        }];
        let long = "x".repeat(300);
        let records = vec![
            rec("A", "www.example.com", "192.0.2.1", None),
            rec("MX", "example.com", "mail.example.com", Some(10)),
            rec("TXT", "example.com", &format!("say \"hi\" {long}"), None),
            rec("CNAME", "app.example.com", "www.example.com", None),
        ];
        let out = render(&ZoneFile {
            zone: "example.com",
            serial: 2_026_100_901,
            default_ttl: 3600,
            admin_email: "first.last@example.org",
            name_servers: &ns,
            records: &records,
        });
        assert!(out.contains("$ORIGIN example.com.\n$TTL 3600\n"));
        assert!(out.contains("SOA\tns1.example.com. first\\.last.example.org. ("));
        assert!(out.contains("2026100901\t; serial"));
        assert!(out.contains("@\t3600\tIN\tNS\tns1.example.com.\n"));
        assert!(out.contains("ns1.example.com.\t3600\tIN\tA\t203.0.113.7\n"));
        assert!(out.contains("www.example.com.\t300\tIN\tA\t192.0.2.1\n"));
        assert!(out.contains("example.com.\t300\tIN\tMX\t10 mail.example.com.\n"));
        assert!(out.contains("app.example.com.\t300\tIN\tCNAME\twww.example.com.\n"));
        assert!(out.contains("\"say \\\"hi\\\" xxx"));
        let txt_line = out.lines().find(|l| l.contains("TXT")).unwrap();
        assert_eq!(
            txt_line.matches("\" \"").count(),
            1,
            "split into two strings"
        );
    }

    #[test]
    fn txt_chunks_are_at_most_255_bytes() {
        let q = quote_txt(&"é".repeat(200));
        for chunk in q.split("\" \"") {
            assert!(chunk.trim_matches('"').len() <= 255);
        }
    }
}
