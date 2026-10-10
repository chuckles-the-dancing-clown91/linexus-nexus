//! Cloudflare API v4 client: token verification, accounts, zones, DNS records
//! and Registrar. Every answer is the v4 envelope `{success, errors, result,
//! result_info}`; a `success: false` is an error whatever the HTTP status.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use loco_rs::app::AppContext;
use reqwest::Method;
use serde_json::{json, Value};

use super::{require_credentials, Api, Credentials, ProviderError, ProviderResult, CLOUDFLARE};
use crate::models::system_tokens::hash_token;

const MAX_PAGES: u64 = 200;

/// Cloudflare error codes that mean "already exists".
const CONFLICT_CODES: [i64; 4] = [1061, 81053, 81057, 81058];

/// "account with given Tag doesn't exist": a well-formed account id that is not
/// one of the token's accounts (a zone id, another account's id, a typo).
const UNKNOWN_ACCOUNT_CODE: i64 = 70503;

/// A Cloudflare client bound to one token (and possibly an account).
#[derive(Clone)]
pub struct Cloudflare {
    api: Api,
    account_id: Option<String>,
}

fn discovered_accounts() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Cloudflare ids are 32 hex characters; anything else never reaches a path.
#[must_use]
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric())
}

impl Cloudflare {
    pub fn new(creds: &Credentials) -> ProviderResult<Self> {
        Ok(Self {
            api: Api::new(CLOUDFLARE, creds.token.clone())?,
            account_id: creds.account_id.clone(),
        })
    }

    pub async fn from_ctx(ctx: &AppContext) -> ProviderResult<Self> {
        Self::new(&require_credentials(ctx, CLOUDFLARE).await?)
    }

    fn envelope_error(&self, body: &Value, status: u16) -> ProviderError {
        let codes: Vec<i64> = body
            .get("errors")
            .and_then(Value::as_array)
            .map(|es| {
                es.iter()
                    .filter_map(|e| e.get("code").and_then(Value::as_i64))
                    .collect()
            })
            .unwrap_or_default();
        let message = self.api.scrub(&super::error_message(body));
        if codes.iter().any(|c| CONFLICT_CODES.contains(c)) {
            ProviderError::Conflict(message)
        } else {
            ProviderError::Rejected { status, message }
        }
    }

    /// One call; returns the whole envelope after checking `success`.
    async fn call(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> ProviderResult<Value> {
        let answer = match self.api.send(method, path, query, body).await {
            Ok(a) => a,
            // The generic client only knows HTTP statuses; Cloudflare says
            // "already exists" with a code in a 400 (rendered "(code N)").
            Err(ProviderError::Rejected { status, message }) => {
                if CONFLICT_CODES
                    .iter()
                    .any(|c| message.contains(&format!("(code {c})")))
                {
                    return Err(ProviderError::Conflict(message));
                }
                if message.contains(&format!("(code {UNKNOWN_ACCOUNT_CODE})")) {
                    // The token was accepted; the account id it was sent with
                    // is not one of its accounts. Say so in words the person
                    // who typed it can act on.
                    return Err(ProviderError::Rejected {
                        status,
                        message: format!(
                            "Cloudflare accepted the token but does not know the account id Nexus is set to use \
                             (it is not an account this token can see). Clear the Account ID under Write access so \
                             Nexus finds the account from the token, or paste the right one (Cloudflare dashboard → \
                             Account home → ⋮ → Copy account ID). [{message}]"
                        ),
                    });
                }
                return Err(ProviderError::Rejected { status, message });
            }
            Err(e) => return Err(e),
        };
        if answer.body.get("success").and_then(Value::as_bool) == Some(false) {
            return Err(self.envelope_error(&answer.body, answer.status));
        }
        if !answer.body.is_object() {
            return Err(ProviderError::Unreachable(
                "cloudflare answered without the v4 envelope".into(),
            ));
        }
        Ok(answer.body)
    }

    async fn result(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> ProviderResult<Value> {
        Ok(self
            .call(method, path, &[], body)
            .await?
            .get("result")
            .cloned()
            .unwrap_or(Value::Null))
    }

    /// Every page of a list endpoint.
    async fn list(
        &self,
        path: &str,
        query: &[(&str, String)],
        per_page: u64,
    ) -> ProviderResult<Vec<Value>> {
        let mut out = Vec::new();
        let mut page = 1;
        loop {
            let mut q = query.to_vec();
            q.push(("page", page.to_string()));
            q.push(("per_page", per_page.to_string()));
            let env = self.call(Method::GET, path, &q, None).await?;
            let items = env
                .get("result")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let n = items.len();
            out.extend(items);
            let total_pages = env
                .pointer("/result_info/total_pages")
                .and_then(Value::as_u64)
                .unwrap_or(1);
            if n == 0 || page >= total_pages || page >= MAX_PAGES {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// `/user/tokens/verify` → the `result` (`{id, status, …}`).
    pub async fn verify_token(&self) -> ProviderResult<Value> {
        self.result(Method::GET, "/user/tokens/verify", None).await
    }

    /// `/accounts`.
    pub async fn accounts(&self) -> ProviderResult<Vec<Value>> {
        self.list("/accounts", &[], 50).await
    }

    /// The account to act in: the configured one, else the only one the
    /// token can see (cached per token). Several visible accounts and none
    /// configured is `NotConfigured` — guessing could act in the wrong one.
    pub async fn account_id(&self) -> ProviderResult<String> {
        if let Some(id) = self.account_id.clone().filter(|s| valid_id(s)) {
            return Ok(id);
        }
        let key = hash_token(self.api.token());
        if let Some(id) = discovered_accounts()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .cloned()
        {
            return Ok(id);
        }
        let accounts = self.accounts().await?;
        let ids: Vec<String> = accounts
            .iter()
            .filter_map(|a| a.get("id").and_then(Value::as_str).map(ToString::to_string))
            .filter(|id| valid_id(id))
            .collect();
        match ids.as_slice() {
            [one] => {
                discovered_accounts()
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(key, one.clone());
                Ok(one.clone())
            }
            [] => Err(ProviderError::NotConfigured(
                "the Cloudflare token can see no account".into(),
            )),
            _ => Err(ProviderError::NotConfigured(
                "the Cloudflare token can see several accounts; set accountId (or CLOUDFLARE_ACCOUNT_ID)".into(),
            )),
        }
    }

    pub async fn zones(&self) -> ProviderResult<Vec<Value>> {
        let account = self.account_id().await.ok();
        let q: Vec<(&str, String)> = account
            .clone()
            .map(|a| vec![("account.id", a)])
            .unwrap_or_default();
        match self.list("/zones", &q, 50).await {
            // The filter only narrows what the token sees; a wrong one must not
            // hide the zones the token can read. The test endpoint reports the
            // wrong id, and anything that writes still names it.
            Err(ProviderError::Rejected { message, .. })
                if account.is_some() && message.contains("Cloudflare accepted the token") =>
            {
                self.list("/zones", &[], 50).await
            }
            other => other,
        }
    }

    /// A read that needs nothing but a working token: one zone, if any.
    /// Account-owned tokens cannot use `/user/tokens/verify` and a token
    /// without "Account Settings: Read" cannot list `/accounts`; both can
    /// still do this, so it answers "does this token work at all".
    pub async fn probe(&self) -> ProviderResult<Vec<Value>> {
        let env = self
            .call(
                Method::GET,
                "/zones",
                &[("per_page", "5".to_string())],
                None,
            )
            .await?;
        Ok(env
            .get("result")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// `/accounts/{id}/tokens/verify`: where an account-owned token is verified.
    pub async fn verify_account_token(&self, account: &str) -> ProviderResult<Value> {
        if !valid_id(account) {
            return Err(ProviderError::NotFound(format!(
                "no such account: {account}"
            )));
        }
        self.result(
            Method::GET,
            &format!("/accounts/{account}/tokens/verify"),
            None,
        )
        .await
    }

    pub async fn zone(&self, id: &str) -> ProviderResult<Value> {
        if !valid_id(id) {
            return Err(ProviderError::NotFound(format!("no such zone: cf:{id}")));
        }
        self.result(Method::GET, &format!("/zones/{id}"), None)
            .await
    }

    pub async fn create_zone(&self, name: &str) -> ProviderResult<Value> {
        let account = self.account_id().await?;
        let body = json!({"name": name, "account": {"id": account}, "type": "full"});
        self.result(Method::POST, "/zones", Some(&body)).await
    }

    pub async fn delete_zone(&self, id: &str) -> ProviderResult<()> {
        if !valid_id(id) {
            return Err(ProviderError::NotFound(format!("no such zone: cf:{id}")));
        }
        self.result(Method::DELETE, &format!("/zones/{id}"), None)
            .await?;
        Ok(())
    }

    pub async fn records(
        &self,
        zone: &str,
        rtype: Option<&str>,
        name: Option<&str>,
    ) -> ProviderResult<Vec<Value>> {
        if !valid_id(zone) {
            return Err(ProviderError::NotFound(format!("no such zone: cf:{zone}")));
        }
        let mut q: Vec<(&str, String)> = Vec::new();
        if let Some(t) = rtype {
            q.push(("type", t.to_string()));
        }
        if let Some(n) = name {
            q.push(("name", n.to_string()));
        }
        self.list(&format!("/zones/{zone}/dns_records"), &q, 100)
            .await
    }

    pub async fn record(&self, zone: &str, id: &str) -> ProviderResult<Value> {
        if !valid_id(zone) || !valid_id(id) {
            return Err(ProviderError::NotFound(format!("no such record: {id}")));
        }
        self.result(
            Method::GET,
            &format!("/zones/{zone}/dns_records/{id}"),
            None,
        )
        .await
    }

    pub async fn create_record(&self, zone: &str, body: &Value) -> ProviderResult<Value> {
        if !valid_id(zone) {
            return Err(ProviderError::NotFound(format!("no such zone: cf:{zone}")));
        }
        self.result(
            Method::POST,
            &format!("/zones/{zone}/dns_records"),
            Some(body),
        )
        .await
    }

    pub async fn update_record(&self, zone: &str, id: &str, body: &Value) -> ProviderResult<Value> {
        if !valid_id(zone) || !valid_id(id) {
            return Err(ProviderError::NotFound(format!("no such record: {id}")));
        }
        self.result(
            Method::PATCH,
            &format!("/zones/{zone}/dns_records/{id}"),
            Some(body),
        )
        .await
    }

    pub async fn delete_record(&self, zone: &str, id: &str) -> ProviderResult<()> {
        if !valid_id(zone) || !valid_id(id) {
            return Err(ProviderError::NotFound(format!("no such record: {id}")));
        }
        self.result(
            Method::DELETE,
            &format!("/zones/{zone}/dns_records/{id}"),
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn registrar_domains(&self) -> ProviderResult<Vec<Value>> {
        let account = self.account_id().await?;
        self.list(&format!("/accounts/{account}/registrar/domains"), &[], 50)
            .await
    }

    pub async fn registrar_domain(&self, name: &str) -> ProviderResult<Value> {
        let account = self.account_id().await?;
        self.result(
            Method::GET,
            &format!("/accounts/{account}/registrar/domains/{name}"),
            None,
        )
        .await
    }

    pub async fn update_registrar_domain(&self, name: &str, body: &Value) -> ProviderResult<Value> {
        let account = self.account_id().await?;
        self.result(
            Method::PUT,
            &format!("/accounts/{account}/registrar/domains/{name}"),
            Some(body),
        )
        .await
    }
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `Domain` shape (§6). `zone_id` is the matching `cf:` zone, or `""`.
#[must_use]
pub fn domain_json(d: &Value, zone_id: &str) -> Value {
    let name = ["name", "domain_name", "id"]
        .iter()
        .map(|k| str_of(d, k))
        .find(|s| s.contains('.'))
        .unwrap_or_default();
    let status = {
        let s = str_of(d, "status");
        if s.is_empty() {
            match d.get("registry_statuses") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(a)) => a
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(","),
                _ => String::new(),
            }
        } else {
            s
        }
    };
    let expires = d
        .get("expires_at")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map_or(Value::Null, |s| json!(s));
    json!({
        "name": name,
        "registrar": "cloudflare",
        "status": status,
        "expiresAt": expires,
        "autoRenew": d.get("auto_renew").and_then(Value::as_bool).unwrap_or(false),
        "locked": d.get("locked").and_then(Value::as_bool).unwrap_or(false),
        "privacy": d.get("privacy").and_then(Value::as_bool).unwrap_or(false),
        "nameServers": d.get("name_servers").cloned().filter(Value::is_array).unwrap_or(json!([])),
        "zoneId": zone_id,
    })
}
