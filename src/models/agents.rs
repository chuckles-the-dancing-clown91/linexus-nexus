use loco_rs::prelude::*;
use sea_orm::ActiveValue;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use super::_entities::agents::{self, ActiveModel, Entity, Model};

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct EnrollAgentParams {
    pub hostname: String,
    pub hostgroup: Option<String>,
    pub capability_manifest: Option<String>,
    /// `/etc/machine-id`; re-adopts an existing agent with the same one.
    #[serde(default)]
    pub machine_id: Option<String>,
    /// The environment to enroll into (an enrollment token's wins).
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub enrollment_token_id: Option<Uuid>,
    /// The Hub's ids carried on the enrollment token.
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

/// Plaintext prefix of an agent's own credential.
pub const CREDENTIAL_PREFIX: &str = "nxa_";

/// The environment and tracking state the Hub has decided for a machine.
///
/// This is not a fact the agent reports — it is policy pushed down to it, and
/// it is stored here so an agent that is offline (or reinstalled next month)
/// still learns what it is the moment it enrolls, instead of coming back as an
/// anonymous production node that immediately starts paging somebody.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SetEnvironmentParams {
    pub environment: String,
    pub monitored: bool,
    #[serde(default)]
    pub note: Option<String>,
}

/// The default environment for an agent nobody has classified. Production,
/// because the safe reading of an unlabelled machine is that it matters.
pub const DEFAULT_ENVIRONMENT: &str = "production";

/// Facts an agent reports after enrolling (or on any subsequent scan). Every
/// field is optional so a partial report only touches what it carries.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct ReportFactsParams {
    pub hostname: Option<String>,
    pub hostgroup: Option<String>,
    pub os: Option<String>,
    pub kernel: Option<String>,
    pub arch: Option<String>,
    pub cpu_cores: Option<i32>,
    pub memory_mb: Option<i32>,
    pub disk_gb: Option<i32>,
    pub agent_version: Option<String>,
    pub uptime_seconds: Option<i64>,
    pub capability_manifest: Option<String>,
    // Richer facts; JSON values stored as text.
    #[serde(default)]
    pub machine_id: Option<String>,
    #[serde(default)]
    pub public_ip: Option<String>,
    #[serde(default)]
    pub interfaces: Option<serde_json::Value>,
    #[serde(default)]
    pub listening: Option<serde_json::Value>,
    #[serde(default)]
    pub services: Option<serde_json::Value>,
    #[serde(default)]
    pub packages: Option<serde_json::Value>,
    #[serde(default)]
    pub dns_server: Option<serde_json::Value>,
}

/// Decode a stored JSON text column, `default` when absent or unreadable.
#[must_use]
pub fn json_column(raw: Option<&str>, default: serde_json::Value) -> serde_json::Value {
    raw.and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(default)
}

impl Model {
    /// The oldest agent that reported `machine_id`.
    pub async fn find_by_machine_id(
        db: &DatabaseConnection,
        machine_id: &str,
    ) -> ModelResult<Option<Self>> {
        use sea_orm::QueryOrder;
        Ok(agents::Entity::find()
            .filter(agents::Column::MachineId.eq(machine_id))
            .order_by_asc(agents::Column::Id)
            .one(db)
            .await?)
    }

    /// The agent holding the credential whose SHA-256 is `hash`.
    pub async fn find_by_credential_hash(
        db: &DatabaseConnection,
        hash: &str,
    ) -> ModelResult<Option<Self>> {
        Ok(agents::Entity::find()
            .filter(agents::Column::CredentialHash.eq(hash))
            .one(db)
            .await?)
    }

    /// Every agent in `hostgroup`.
    pub async fn find_by_hostgroup(
        db: &DatabaseConnection,
        hostgroup: &str,
    ) -> ModelResult<Vec<Self>> {
        use sea_orm::QueryOrder;
        Ok(agents::Entity::find()
            .filter(agents::Column::Hostgroup.eq(hostgroup))
            .order_by_asc(agents::Column::Id)
            .all(db)
            .await?)
    }

    /// Issue a fresh `nxa_` credential, replacing any previous one. Returns
    /// the updated row and the one-time plaintext.
    pub async fn rotate_credential(self, db: &DatabaseConnection) -> ModelResult<(Self, String)> {
        let plaintext = format!(
            "{CREDENTIAL_PREFIX}{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        );
        let mut active: agents::ActiveModel = self.into();
        active.credential_hash =
            ActiveValue::set(Some(crate::models::system_tokens::hash_token(&plaintext)));
        Ok((active.update(db).await?, plaintext))
    }

    /// Re-adopt this agent for a machine enrolling again: keep the agent id,
    /// take the new identity and placement (facts are kept until the next
    /// report).
    pub async fn readopt(
        self,
        db: &DatabaseConnection,
        params: &EnrollAgentParams,
    ) -> ModelResult<Self> {
        let mut active: agents::ActiveModel = self.into();
        active.hostname = ActiveValue::set(params.hostname.clone());
        if params.hostgroup.is_some() {
            active.hostgroup = ActiveValue::set(params.hostgroup.clone());
        }
        if let Some(env) = params.environment.as_ref().filter(|e| !e.trim().is_empty()) {
            active.environment = ActiveValue::set(env.trim().to_lowercase());
        }
        if params.capability_manifest.is_some() {
            active.capability_manifest = ActiveValue::set(params.capability_manifest.clone());
        }
        if params.enrollment_token_id.is_some() {
            active.enrollment_token_id = ActiveValue::set(params.enrollment_token_id);
        }
        if let Some(m) = &params.metadata {
            active.metadata = ActiveValue::set(Some(m.to_string()));
        }
        active.machine_id = ActiveValue::set(params.machine_id.clone());
        active.enrolled_at = ActiveValue::set(Some(chrono::Local::now().into()));
        Ok(active.update(db).await?)
    }

    /// Remember the last `install_dns_server` task sent to this agent.
    pub async fn set_dns_install_task(
        self,
        db: &DatabaseConnection,
        task_id: &str,
    ) -> ModelResult<Self> {
        let mut active: agents::ActiveModel = self.into();
        active.dns_install_task_id = ActiveValue::set(Some(task_id.to_string()));
        Ok(active.update(db).await?)
    }

    /// Every IPv4/IPv6 address this agent reported (public IP and the
    /// interface addresses, without prefix lengths).
    #[must_use]
    pub fn addresses(&self) -> Vec<String> {
        let mut out: Vec<String> = self.public_ip.iter().cloned().collect();
        if let serde_json::Value::Array(ifaces) =
            json_column(self.interfaces.as_deref(), serde_json::Value::Null)
        {
            for iface in ifaces {
                if let Some(addrs) = iface.get("addresses").and_then(serde_json::Value::as_array) {
                    for a in addrs.iter().filter_map(serde_json::Value::as_str) {
                        let ip = a.split('/').next().unwrap_or(a).to_string();
                        if !out.contains(&ip) {
                            out.push(ip);
                        }
                    }
                }
            }
        }
        out
    }

    /// The address other servers should use to reach this agent: its public
    /// IP, else its first non-loopback, non-link-local IPv4.
    #[must_use]
    pub fn reachable_ip(&self) -> Option<String> {
        if let Some(ip) = self.public_ip.clone().filter(|s| !s.is_empty()) {
            return Some(ip);
        }
        self.addresses().into_iter().find(|a| {
            a.parse::<std::net::Ipv4Addr>()
                .is_ok_and(|ip| !ip.is_loopback() && !ip.is_link_local())
        })
    }

    /// Find agent by UUID
    pub async fn find_by_agent_id(db: &DatabaseConnection, agent_id: &Uuid) -> ModelResult<Self> {
        let agent = agents::Entity::find()
            .filter(
                model::query::condition()
                    .eq(agents::Column::AgentId, *agent_id)
                    .build(),
            )
            .one(db)
            .await?;
        agent.ok_or_else(|| ModelError::EntityNotFound)
    }

    /// Find an agent by hostname.
    ///
    /// Daedalus IT targets a machine by its agent id when it has one and falls
    /// back to the hostname when it doesn't, so both lookups have to exist for
    /// a targeted intent to reach the right inventory row.
    pub async fn find_by_hostname(db: &DatabaseConnection, hostname: &str) -> ModelResult<Self> {
        let agent = agents::Entity::find()
            .filter(
                model::query::condition()
                    .eq(agents::Column::Hostname, hostname)
                    .build(),
            )
            .one(db)
            .await?;
        agent.ok_or_else(|| ModelError::EntityNotFound)
    }

    /// Find all agents
    pub async fn find_all(db: &DatabaseConnection) -> ModelResult<Vec<Self>> {
        Ok(agents::Entity::find().all(db).await?)
    }

    /// Enroll a new agent
    pub async fn enroll(db: &DatabaseConnection, params: &EnrollAgentParams) -> ModelResult<Self> {
        let agent = agents::ActiveModel {
            agent_id: ActiveValue::set(Uuid::new_v4()),
            hostname: ActiveValue::set(params.hostname.clone()),
            status: ActiveValue::set("enrolled".to_string()),
            capability_manifest: ActiveValue::set(params.capability_manifest.clone()),
            hostgroup: ActiveValue::set(params.hostgroup.clone()),
            enrolled_at: ActiveValue::set(Some(chrono::Local::now().into())),
            last_heartbeat_at: ActiveValue::set(None),
            // A machine enrolls as production and tracked. Both are set
            // explicitly rather than left to the column default so the record
            // returned to the enrolling agent carries the real values — an
            // agent that reads `monitored: false` out of an unset field would
            // go quiet the moment it came online.
            environment: ActiveValue::set(
                params
                    .environment
                    .as_ref()
                    .map(|e| e.trim().to_lowercase())
                    .filter(|e| !e.is_empty())
                    .unwrap_or_else(|| DEFAULT_ENVIRONMENT.to_string()),
            ),
            monitored: ActiveValue::set(true),
            monitor_note: ActiveValue::set(None),
            machine_id: ActiveValue::set(params.machine_id.clone()),
            enrollment_token_id: ActiveValue::set(params.enrollment_token_id),
            metadata: ActiveValue::set(params.metadata.as_ref().map(ToString::to_string)),
            ..Default::default()
        }
        .insert(db)
        .await?;

        Ok(agent)
    }

    /// Apply an environment / tracking decision to this agent.
    ///
    /// Turning tracking back on clears the note: a stale "decommissioning the
    /// old mail relay" left on a live machine is worse than no note at all.
    pub async fn set_environment(
        self,
        db: &DatabaseConnection,
        params: &SetEnvironmentParams,
    ) -> ModelResult<Self> {
        let environment = if params.environment.trim().is_empty() {
            DEFAULT_ENVIRONMENT.to_string()
        } else {
            params.environment.trim().to_lowercase()
        };
        let note = if params.monitored {
            None
        } else {
            params
                .note
                .as_ref()
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
        };

        let mut active: agents::ActiveModel = self.into();
        active.environment = ActiveValue::set(environment);
        active.monitored = ActiveValue::set(params.monitored);
        active.monitor_note = ActiveValue::set(note);
        active.environment_updated_at = ActiveValue::set(Some(chrono::Local::now().into()));
        Ok(active.update(db).await?)
    }

    /// Record a heartbeat
    pub async fn heartbeat(self, db: &DatabaseConnection) -> ModelResult<Self> {
        let mut active: agents::ActiveModel = self.into();
        active.last_heartbeat_at = ActiveValue::set(Some(chrono::Local::now().into()));
        active.status = ActiveValue::set("healthy".to_string());
        Ok(active.update(db).await?)
    }

    /// Apply a facts report: update whatever fields are present, mark the agent
    /// healthy, and stamp the heartbeat. A report is also a liveness signal.
    pub async fn report_facts(
        self,
        db: &DatabaseConnection,
        params: &ReportFactsParams,
    ) -> ModelResult<Self> {
        let mut active: agents::ActiveModel = self.into();
        if let Some(v) = &params.hostname {
            active.hostname = ActiveValue::set(v.clone());
        }
        if params.hostgroup.is_some() {
            active.hostgroup = ActiveValue::set(params.hostgroup.clone());
        }
        if params.os.is_some() {
            active.os = ActiveValue::set(params.os.clone());
        }
        if params.kernel.is_some() {
            active.kernel = ActiveValue::set(params.kernel.clone());
        }
        if params.arch.is_some() {
            active.arch = ActiveValue::set(params.arch.clone());
        }
        if params.cpu_cores.is_some() {
            active.cpu_cores = ActiveValue::set(params.cpu_cores);
        }
        if params.memory_mb.is_some() {
            active.memory_mb = ActiveValue::set(params.memory_mb);
        }
        if params.disk_gb.is_some() {
            active.disk_gb = ActiveValue::set(params.disk_gb);
        }
        if params.agent_version.is_some() {
            active.agent_version = ActiveValue::set(params.agent_version.clone());
        }
        if params.uptime_seconds.is_some() {
            active.uptime_seconds = ActiveValue::set(params.uptime_seconds);
        }
        if params.capability_manifest.is_some() {
            active.capability_manifest = ActiveValue::set(params.capability_manifest.clone());
        }
        if let Some(v) = params.machine_id.as_ref().filter(|v| !v.trim().is_empty()) {
            active.machine_id = ActiveValue::set(Some(v.trim().to_string()));
        }
        if params.public_ip.is_some() {
            active.public_ip = ActiveValue::set(params.public_ip.clone());
        }
        let json = |v: &Option<serde_json::Value>| v.as_ref().map(ToString::to_string);
        if params.interfaces.is_some() {
            active.interfaces = ActiveValue::set(json(&params.interfaces));
        }
        if params.listening.is_some() {
            active.listening = ActiveValue::set(json(&params.listening));
        }
        if params.services.is_some() {
            active.services = ActiveValue::set(json(&params.services));
        }
        if params.packages.is_some() {
            active.packages = ActiveValue::set(json(&params.packages));
        }
        if params.dns_server.is_some() {
            active.dns_server = ActiveValue::set(json(&params.dns_server));
        }
        active.facts_at = ActiveValue::set(Some(chrono::Local::now().into()));
        active.status = ActiveValue::set("healthy".to_string());
        active.last_heartbeat_at = ActiveValue::set(Some(chrono::Local::now().into()));
        Ok(active.update(db).await?)
    }
}
