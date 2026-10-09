//! # Infrastructure providers
//!
//! Nexus holds the write credentials for the infrastructure we run on and is
//! the only component that talks to it: DigitalOcean (compute, volumes, load
//! balancers), Cloudflare (DNS and Registrar) and BIND servers hosted by our
//! own agents. The contract is `docs/PROVIDERS.md`.
//!
//! Security properties every client here keeps:
//! * API **bases come only from the environment** (`DIGITALOCEAN_API_BASE`,
//!   `CLOUDFLARE_API_BASE`), read at call time, so nobody with API access can
//!   point a stored token at a host of their choosing.
//! * **Redirects are never followed** (a redirect would carry the bearer
//!   token to wherever it points) and every call has a 20 s timeout.
//! * The token is **scrubbed from every error** text before it leaves this
//!   module.
//! * Environment credentials (`DIGITALOCEAN_TOKEN`, `CLOUDFLARE_API_TOKEN`,
//!   `CLOUDFLARE_ACCOUNT_ID`) win over stored, sealed ones.
//! * Outbound proxies are not used unless `NEXUS_PROVIDER_PROXY` names one.

pub mod bind;
pub mod cloudflare;
pub mod digitalocean;
pub mod dns;

use std::time::Duration;

use loco_rs::app::AppContext;
use serde_json::Value;

use crate::models::provider_credentials;
use crate::secrets;

pub const DIGITALOCEAN: &str = "digitalocean";
pub const CLOUDFLARE: &str = "cloudflare";
pub const BIND: &str = "bind";

/// Providers that take credentials.
pub const CREDENTIALED: [&str; 2] = [DIGITALOCEAN, CLOUDFLARE];

const TIMEOUT: Duration = Duration::from_secs(20);
const MAX_ERROR_DETAIL: usize = 1000;
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Why a provider call did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// No credentials (or they cannot be unsealed).
    NotConfigured(String),
    /// The provider answered 4xx (other than 404 / a conflict).
    Rejected { status: u16, message: String },
    /// The provider answered 404, or the thing does not exist.
    NotFound(String),
    /// The provider says it already exists.
    Conflict(String),
    /// Timed out, 5xx, a redirect, or a body we could not use.
    Unreachable(String),
    /// Our own validation of the request failed.
    Invalid(String),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured(m)
            | Self::NotFound(m)
            | Self::Conflict(m)
            | Self::Unreachable(m)
            | Self::Invalid(m) => f.write_str(m),
            Self::Rejected { status, message } => write!(f, "{status}: {message}"),
        }
    }
}

impl std::error::Error for ProviderError {}

pub type ProviderResult<T> = Result<T, ProviderError>;

/// Where a provider's credentials came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Env,
    Stored,
}

impl Source {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::Stored => "stored",
        }
    }
}

/// A resolved credential. `Debug` never prints the token.
#[derive(Clone)]
pub struct Credentials {
    pub token: String,
    pub account_id: Option<String>,
    pub source: Source,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("token", &"[redacted]")
            .field("account_id", &self.account_id)
            .field("source", &self.source)
            .finish()
    }
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The token env var for a provider key.
#[must_use]
pub fn token_env(key: &str) -> Option<&'static str> {
    match key {
        DIGITALOCEAN => Some("DIGITALOCEAN_TOKEN"),
        CLOUDFLARE => Some("CLOUDFLARE_API_TOKEN"),
        _ => None,
    }
}

/// Resolve a provider's credentials: environment first, then the sealed
/// stored token. `Ok(None)` when there are none; `NotConfigured` when a
/// stored token exists but cannot be unsealed.
pub async fn credentials(ctx: &AppContext, key: &str) -> ProviderResult<Option<Credentials>> {
    if let Some(token) = token_env(key).and_then(env_nonempty) {
        let account_id = if key == CLOUDFLARE {
            env_nonempty("CLOUDFLARE_ACCOUNT_ID")
        } else {
            None
        };
        return Ok(Some(Credentials {
            token,
            account_id,
            source: Source::Env,
        }));
    }
    let row = provider_credentials::Model::find_by_provider(&ctx.db, key)
        .await
        .map_err(|e| ProviderError::Unreachable(format!("database: {e}")))?;
    let Some(row) = row else { return Ok(None) };
    let Some(sealed) = row.sealed_token.as_deref() else {
        return Ok(None);
    };
    let token = secrets::open(&ctx.environment, key, sealed)
        .map_err(|e| ProviderError::NotConfigured(e.to_string()))?;
    Ok(Some(Credentials {
        token,
        account_id: row.account_id.clone().filter(|s| !s.is_empty()),
        source: Source::Stored,
    }))
}

/// Credentials or `NotConfigured`.
pub async fn require_credentials(ctx: &AppContext, key: &str) -> ProviderResult<Credentials> {
    credentials(ctx, key)
        .await?
        .ok_or_else(|| ProviderError::NotConfigured(format!("{key} has no credentials")))
}

/// API base for a provider, from the environment only, read at call time.
#[must_use]
pub fn api_base(key: &str) -> String {
    let (var, default) = match key {
        DIGITALOCEAN => ("DIGITALOCEAN_API_BASE", "https://api.digitalocean.com"),
        _ => (
            "CLOUDFLARE_API_BASE",
            "https://api.cloudflare.com/client/v4",
        ),
    };
    env_nonempty(var)
        .unwrap_or_else(|| default.to_string())
        .trim_end_matches('/')
        .to_string()
}

fn http_client() -> ProviderResult<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("linexus-nexus/", env!("CARGO_PKG_VERSION")));
    builder =
        match env_nonempty("NEXUS_PROVIDER_PROXY") {
            Some(proxy) => builder.proxy(reqwest::Proxy::all(&proxy).map_err(|_| {
                ProviderError::Unreachable("NEXUS_PROVIDER_PROXY is invalid".into())
            })?),
            None => builder.no_proxy(),
        };
    builder
        .build()
        .map_err(|e| ProviderError::Unreachable(format!("http client: {e}")))
}

/// Remove `token` (and anything that looks like a bearer credential) from
/// `text`, and cap its length.
#[must_use]
pub fn scrub(text: &str, token: &str) -> String {
    let mut out = text.to_string();
    if token.len() >= 4 {
        out = out.replace(token, "[redacted]");
    }
    let patterns = [
        r"(?i)bearer\s+[A-Za-z0-9._~+/=\-]{8,}",
        r"dop_v1_[A-Za-z0-9]+",
        r"\bnx[aex]_[A-Za-z0-9]{8,}",
    ];
    for p in patterns {
        if let Ok(re) = regex::Regex::new(p) {
            out = re.replace_all(&out, "[redacted]").into_owned();
        }
    }
    if out.len() > MAX_ERROR_DETAIL {
        let mut cut = MAX_ERROR_DETAIL;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push('…');
    }
    out
}

/// A bearer-authenticated JSON API at a fixed base.
#[derive(Clone)]
pub struct Api {
    base: String,
    token: String,
    http: reqwest::Client,
    name: &'static str,
}

/// A successful answer: status and JSON body (`Null` when empty).
pub struct Answer {
    pub status: u16,
    pub body: Value,
}

impl Api {
    pub fn new(key: &'static str, token: String) -> ProviderResult<Self> {
        Ok(Self {
            base: api_base(key),
            token,
            http: http_client()?,
            name: key,
        })
    }

    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Scrub this API's token out of `text`.
    #[must_use]
    pub fn scrub(&self, text: &str) -> String {
        scrub(text, &self.token)
    }

    /// Send one request. `path` starts with `/` and is appended to the base;
    /// callers only ever build it from validated ids.
    ///
    /// Answers: 2xx → `Ok`; 404 → `NotFound`; 409 → `Conflict`; other 4xx →
    /// `Rejected` with the body (the caller extracts the provider's message);
    /// 3xx, 5xx, transport failures and unreadable bodies → `Unreachable`.
    pub async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> ProviderResult<Answer> {
        let url = format!("{}{}", self.base, path);
        let mut req = self
            .http
            .request(method.clone(), &url)
            .bearer_auth(&self.token)
            .header(reqwest::header::ACCEPT, "application/json");
        if !query.is_empty() {
            req = req.query(query);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await.map_err(|e| {
            let what = if e.is_timeout() {
                "timed out".to_string()
            } else if e.is_connect() {
                "connection failed".to_string()
            } else {
                e.to_string()
            };
            ProviderError::Unreachable(
                self.scrub(&format!("{} {method} {path}: {what}", self.name)),
            )
        })?;
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await.map_err(|e| {
            ProviderError::Unreachable(self.scrub(&format!("{} {method} {path}: {e}", self.name)))
        })?;
        if bytes.len() > MAX_BODY_BYTES {
            return Err(ProviderError::Unreachable(format!(
                "{} answered a body larger than {MAX_BODY_BYTES} bytes",
                self.name
            )));
        }
        let parsed: Option<Value> = if bytes.is_empty() {
            Some(Value::Null)
        } else {
            serde_json::from_slice(&bytes).ok()
        };
        match status {
            200..=299 => parsed.map(|body| Answer { status, body }).ok_or_else(|| {
                ProviderError::Unreachable(format!(
                    "{} {method} {path}: answered a body that is not JSON",
                    self.name
                ))
            }),
            300..=399 => Err(ProviderError::Unreachable(format!(
                "{} {method} {path}: answered a redirect ({status}), which is never followed",
                self.name
            ))),
            400..=499 => {
                let raw = parsed
                    .unwrap_or_else(|| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
                let message = self.scrub(&error_message(&raw));
                Err(match status {
                    404 => ProviderError::NotFound(message),
                    409 => ProviderError::Conflict(message),
                    _ => ProviderError::Rejected { status, message },
                })
            }
            _ => Err(ProviderError::Unreachable(format!(
                "{} {method} {path}: answered {status}",
                self.name
            ))),
        }
    }
}

/// The human message in a provider error body: DigitalOcean's `message`, or
/// Cloudflare's `errors[].message` (with codes), or the raw text.
#[must_use]
pub fn error_message(body: &Value) -> String {
    if let Some(errors) = body.get("errors").and_then(Value::as_array) {
        let parts: Vec<String> = errors
            .iter()
            .map(|e| {
                let msg = e.get("message").and_then(Value::as_str).unwrap_or("error");
                match e.get("code").and_then(Value::as_i64) {
                    Some(code) => format!("{msg} (code {code})"),
                    None => msg.to_string(),
                }
            })
            .collect();
        if !parts.is_empty() {
            return parts.join("; ");
        }
    }
    if let Some(m) = body.get("message").and_then(Value::as_str) {
        return m.to_string();
    }
    match body {
        Value::String(s) => s.chars().take(MAX_ERROR_DETAIL).collect(),
        Value::Null => "no detail".to_string(),
        other => other.to_string(),
    }
}

/// Convert a provider object's snake_case keys to camelCase, recursively.
/// Used for the read-only lists that are passed through mostly as-is.
#[must_use]
pub fn camelize(v: &Value) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (snake_to_camel(k), camelize(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(camelize).collect()),
        other => other.clone(),
    }
}

fn snake_to_camel(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut upper = false;
    for c in s.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_removes_tokens() {
        let token = "cf-secret-token-123456";
        let s = scrub(
            &format!("bad token {token} sent as Bearer {token}; also dop_v1_abcdef0123"),
            token,
        );
        assert!(!s.contains(token));
        assert!(!s.contains("dop_v1_abcdef0123"));
    }

    #[test]
    fn camelize_keys() {
        let v = serde_json::json!({"ip_range": "10.0.0.0/16", "nested": [{"created_at": 1}]});
        assert_eq!(
            camelize(&v),
            serde_json::json!({"ipRange": "10.0.0.0/16", "nested": [{"createdAt": 1}]})
        );
    }
}
