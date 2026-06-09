#![allow(clippy::missing_errors_doc)]
#![allow(clippy::unnecessary_struct_initialization)]
#![allow(clippy::unused_async)]
//! # Nexus Demiurge control surface
//!
//! Real-time Demiurge management: node commissioning, live wallet balances,
//! contribution minting, redemption, and Vicinagora payments. Service routes are
//! guarded by the system-token middleware so trusted publishers (Tea & Madness)
//! can drive them machine-to-machine; administrative routes use a human JWT.

use axum::extract::Path;
use axum::http::HeaderMap;
use loco_rs::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::demiurge;
use crate::middleware::{rbac, system_token};
use crate::models::{contributions, nodes, payments, system_tokens, wallet};

// ----------------------------------------------------------------------------
// Serialization helpers
// ----------------------------------------------------------------------------

fn parse_capabilities(raw: &Option<String>) -> Vec<String> {
    let Some(s) = raw else { return vec![] };
    if let Ok(v) = serde_json::from_str::<Vec<String>>(s) {
        return v;
    }
    s.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

fn parse_node_uuid(raw: &str) -> Result<Uuid> {
    Uuid::parse_str(raw).map_err(|e| loco_rs::Error::BadRequest(format!("invalid node id: {e}")))
}

#[derive(Debug, Serialize)]
pub struct NodeResponse {
    pub node_id: String,
    pub class: String,
    pub label: String,
    pub status: String,
    pub lifecycle_phase: Option<String>,
    pub capabilities: Vec<String>,
    pub source: Option<String>,
    pub external_ref: Option<String>,
    pub commissioned_at: Option<String>,
}

impl From<nodes::Model> for NodeResponse {
    fn from(n: nodes::Model) -> Self {
        Self {
            node_id: n.node_id.to_string(),
            class: n.class,
            label: n.label,
            status: n.status,
            lifecycle_phase: n.lifecycle_phase,
            capabilities: parse_capabilities(&n.capabilities),
            source: n.source,
            external_ref: n.external_ref,
            commissioned_at: n.commissioned_at.map(|d| d.to_rfc3339()),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct LotResponse {
    pub amount: i64,
    pub original_amount: i64,
    pub minted_at_unix: i64,
    pub expires_at_unix: i64,
    pub origin: String,
    pub origin_ref: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WalletResponse {
    pub node_id: String,
    pub balance: i64,
    pub minted_last_7d: i64,
    pub expiring_within_year: i64,
    pub lot_count: usize,
    pub now_unix: i64,
    pub lots: Vec<LotResponse>,
}

#[derive(Debug, Serialize)]
pub struct ContributionResponse {
    pub contribution_id: String,
    pub kind: String,
    pub minutes: i64,
    pub essential: bool,
    pub coverage: bool,
    pub week_index: i64,
    pub minted_amount: i64,
    pub at_unix: i64,
    pub note: Option<String>,
}

impl From<contributions::Model> for ContributionResponse {
    fn from(c: contributions::Model) -> Self {
        Self {
            contribution_id: c.contribution_id.to_string(),
            kind: c.kind,
            minutes: c.minutes,
            essential: c.essential,
            coverage: c.coverage,
            week_index: c.week_index,
            minted_amount: c.minted_amount,
            at_unix: c.at_unix,
            note: c.note,
        }
    }
}

/// The floor is guaranteed by breath, never by balance. This response reads no
/// wallet — it is the same for everyone alive.
#[derive(Debug, Serialize)]
pub struct FloorResponse {
    pub housing: bool,
    pub power: bool,
    pub food: bool,
    pub water: bool,
    pub healthcare: bool,
    pub education: bool,
}

impl Default for FloorResponse {
    fn default() -> Self {
        Self {
            housing: true,
            power: true,
            food: true,
            water: true,
            healthcare: true,
            education: true,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct StandingResponse {
    pub node: NodeResponse,
    pub wallet: WalletResponse,
    pub recent_contributions: Vec<ContributionResponse>,
    pub floor: FloorResponse,
}

#[derive(Debug, Serialize)]
pub struct PaymentResponse {
    pub payment_id: String,
    pub kind: String,
    pub pay_currency: String,
    pub amount_fiat_minor: i64,
    pub currency: String,
    pub fee_fiat_minor: i64,
    pub tax_fiat_minor: i64,
    pub demiurge_amount: i64,
    pub demiurge_from_fees: i64,
    pub status: String,
    pub payee_node_id: Option<String>,
    pub item_ref: Option<String>,
    pub processed_at: Option<String>,
}

impl From<payments::Model> for PaymentResponse {
    fn from(p: payments::Model) -> Self {
        Self {
            payment_id: p.payment_id.to_string(),
            kind: p.kind,
            pay_currency: p.pay_currency,
            amount_fiat_minor: p.amount_fiat_minor,
            currency: p.currency,
            fee_fiat_minor: p.fee_fiat_minor,
            tax_fiat_minor: p.tax_fiat_minor,
            demiurge_amount: p.demiurge_amount,
            demiurge_from_fees: p.demiurge_from_fees,
            status: p.status,
            payee_node_id: p.payee_node_id.map(|u| u.to_string()),
            item_ref: p.item_ref,
            processed_at: p.processed_at.map(|d| d.to_rfc3339()),
        }
    }
}

async fn wallet_response(ctx: &AppContext, node_id: Uuid) -> Result<WalletResponse> {
    let now = chrono::Utc::now().timestamp();
    let outlook = wallet::outlook(&ctx.db, node_id, now).await?;
    let lots = wallet::lots(&ctx.db, node_id).await?;
    let lots: Vec<LotResponse> = lots
        .into_iter()
        .filter(|l| l.amount > 0 && now < l.expires_at_unix)
        .map(|l| LotResponse {
            amount: l.amount,
            original_amount: l.original_amount,
            minted_at_unix: l.minted_at_unix,
            expires_at_unix: l.expires_at_unix,
            origin: l.origin,
            origin_ref: l.origin_ref,
        })
        .collect();
    Ok(WalletResponse {
        node_id: outlook.node_id.to_string(),
        balance: outlook.balance,
        minted_last_7d: outlook.minted_last_7d,
        expiring_within_year: outlook.expiring_within_year,
        lot_count: outlook.lot_count,
        now_unix: outlook.now_unix,
        lots,
    })
}

// ----------------------------------------------------------------------------
// Node endpoints
// ----------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateNodeRequest {
    #[serde(default = "default_human")]
    pub class: String,
    pub label: String,
    #[serde(default)]
    pub lifecycle_phase: Option<String>,
    #[serde(default)]
    pub capabilities: Option<Vec<String>>,
    #[serde(default)]
    pub public_key: Option<String>,
    #[serde(default)]
    pub owner_user_id: Option<i32>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub external_ref: Option<String>,
}

fn default_human() -> String {
    "human".to_string()
}

/// Commission a node (service). Idempotent on `source`+`external_ref`, so a
/// publisher can safely retry the "account created" push.
pub async fn create_node(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Json(req): Json<CreateNodeRequest>,
) -> Result<Response> {
    system_token::require(&ctx, &headers, "nodes:create").await?;

    let capabilities = req
        .capabilities
        .as_ref()
        .map(|c| serde_json::to_string(c).unwrap_or_default());

    let params = nodes::CommissionParams {
        class: req.class,
        label: req.label,
        lifecycle_phase: req.lifecycle_phase.or_else(|| Some("labor".to_string())),
        capabilities,
        public_key: req.public_key,
        owner_user_id: req.owner_user_id,
        source: req.source,
        external_ref: req.external_ref,
    };

    let node = nodes::Model::commission(&ctx.db, &params).await?;
    format::json(NodeResponse::from(node))
}

pub async fn list_nodes(State(ctx): State<AppContext>, headers: HeaderMap) -> Result<Response> {
    system_token::require(&ctx, &headers, "nodes:read").await?;
    let all = nodes::Model::find_all(&ctx.db).await?;
    let out: Vec<NodeResponse> = all.into_iter().map(NodeResponse::from).collect();
    format::json(out)
}

pub async fn get_node(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(node_id): Path<String>,
) -> Result<Response> {
    system_token::require(&ctx, &headers, "nodes:read").await?;
    let uuid = parse_node_uuid(&node_id)?;
    let node = nodes::Model::find_by_node_id(&ctx.db, &uuid).await?;
    format::json(NodeResponse::from(node))
}

/// Live wallet for a node.
pub async fn get_wallet(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(node_id): Path<String>,
) -> Result<Response> {
    system_token::require(&ctx, &headers, "wallet:read").await?;
    let uuid = parse_node_uuid(&node_id)?;
    nodes::Model::find_by_node_id(&ctx.db, &uuid).await?; // 404 if unknown
    format::json(wallet_response(&ctx, uuid).await?)
}

/// Aggregate resident standing: node, wallet, recent contributions, floor.
pub async fn get_standing(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(node_id): Path<String>,
) -> Result<Response> {
    system_token::require(&ctx, &headers, "wallet:read").await?;
    let uuid = parse_node_uuid(&node_id)?;
    let node = nodes::Model::find_by_node_id(&ctx.db, &uuid).await?;
    let wallet = wallet_response(&ctx, uuid).await?;
    let recent = contributions::Model::recent_for_node(&ctx.db, uuid, 20).await?;
    let recent_contributions = recent.into_iter().map(ContributionResponse::from).collect();
    format::json(StandingResponse {
        node: NodeResponse::from(node),
        wallet,
        recent_contributions,
        floor: FloorResponse::default(),
    })
}

/// Sweep decayed lots and report the amount lost to decay.
pub async fn sweep_wallet(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Path(node_id): Path<String>,
) -> Result<Response> {
    system_token::require(&ctx, &headers, "wallet:write").await?;
    let uuid = parse_node_uuid(&node_id)?;
    let now = chrono::Utc::now().timestamp();
    let decayed = wallet::decay_sweep(&ctx.db, uuid, now).await?;
    format::json(serde_json::json!({ "node_id": node_id, "decayed": decayed }))
}

// ----------------------------------------------------------------------------
// Minting & redemption
// ----------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ContributionRequest {
    pub node_id: String,
    pub kind: String,
    pub minutes: i64,
    #[serde(default)]
    pub essential: bool,
    #[serde(default)]
    pub coverage: bool,
    #[serde(default)]
    pub week_index: i64,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub at_unix: Option<i64>,
}

/// Record a contribution and mint the resulting Demiurge. Only breath mints.
pub async fn record_contribution(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Json(req): Json<ContributionRequest>,
) -> Result<Response> {
    system_token::require(&ctx, &headers, "demiurge:mint").await?;
    let uuid = parse_node_uuid(&req.node_id)?;
    nodes::Model::find_by_node_id(&ctx.db, &uuid).await?;

    let params = contributions::RecordParams {
        node_id: uuid,
        kind: req.kind,
        minutes: req.minutes,
        essential: req.essential,
        coverage: req.coverage,
        week_index: req.week_index,
        note: req.note,
        at_unix: req.at_unix,
    };
    let (row, minted) = contributions::Model::record(&ctx.db, &params).await?;
    format::json(serde_json::json!({
        "contribution": ContributionResponse::from(row),
        "minted": minted,
        "wallet": wallet_response(&ctx, uuid).await?,
    }))
}

#[derive(Debug, Deserialize)]
pub struct RedeemRequest {
    pub node_id: String,
    pub amount: i64,
    #[serde(default)]
    pub memo: Option<String>,
}

/// Redeem (spend) Demiurge from a node's wallet into a sink. The floor is never
/// on the line: redemption can only fail to buy a good, never to keep a person.
pub async fn redeem(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Json(req): Json<RedeemRequest>,
) -> Result<Response> {
    system_token::require(&ctx, &headers, "demiurge:redeem").await?;
    let uuid = parse_node_uuid(&req.node_id)?;
    nodes::Model::find_by_node_id(&ctx.db, &uuid).await?;
    let now = chrono::Utc::now().timestamp();
    let outcome = wallet::spend(&ctx.db, uuid, req.amount, now).await?;
    let _ = &req.memo;
    format::json(serde_json::json!({
        "node_id": req.node_id,
        "outcome": outcome,
        "wallet": wallet_response(&ctx, uuid).await?,
    }))
}

// ----------------------------------------------------------------------------
// Payments
// ----------------------------------------------------------------------------

pub async fn process_payment(
    State(ctx): State<AppContext>,
    headers: HeaderMap,
    Json(req): Json<payments::ProcessParams>,
) -> Result<Response> {
    system_token::require(&ctx, &headers, "payments:process").await?;
    let payment = payments::Model::process(&ctx.db, &req).await?;
    format::json(PaymentResponse::from(payment))
}

pub async fn list_payments(State(ctx): State<AppContext>, headers: HeaderMap) -> Result<Response> {
    system_token::require(&ctx, &headers, "payments:read").await?;
    let all = payments::Model::find_all(&ctx.db, 200).await?;
    let out: Vec<PaymentResponse> = all.into_iter().map(PaymentResponse::from).collect();
    format::json(out)
}

// ----------------------------------------------------------------------------
// System token issuance (human admin only)
// ----------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct IssueTokenRequest {
    pub service: String,
    /// Comma-separated scopes; defaults to the publisher scope bundle.
    #[serde(default)]
    pub scopes: Option<String>,
}

/// Issue a system token for a service. Returns the plaintext exactly once.
/// Requires a human admin JWT with the `system:tokens` permission.
pub async fn issue_token(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
    Json(req): Json<IssueTokenRequest>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "system:tokens").await?;

    let scopes = req.scopes.unwrap_or_else(|| {
        "nodes:create,nodes:read,wallet:read,wallet:write,demiurge:mint,demiurge:redeem,payments:process,payments:read".to_string()
    });
    let (record, plaintext) = system_tokens::Model::issue(&ctx.db, &req.service, &scopes).await?;
    format::json(serde_json::json!({
        "token_id": record.token_id.to_string(),
        "service": record.service,
        "scopes": record.scope_list(),
        "token": plaintext,
        "note": "store this token now; it is not recoverable",
        "header": system_token::HEADER,
    }))
}

/// Public, unauthenticated metadata: the canonical Demiurge parameters, so a
/// client can render rates without hardcoding them.
pub async fn parameters(State(_ctx): State<AppContext>) -> Result<Response> {
    use demiurge::ContributionKind::*;
    format::json(serde_json::json!({
        "seconds_per_year": demiurge::SECONDS_PER_YEAR,
        "expiry_secs": demiurge::DEMIURGE_EXPIRY_SECS,
        "weekly_expectation_hours": demiurge::WEEKLY_EXPECTATION_HOURS,
        "overtime_bps": demiurge::OVERTIME_BPS,
        "essential_bps": demiurge::ESSENTIAL_BPS,
        "coverage_bps": demiurge::COVERAGE_BPS,
        "rates": {
            "labor": Labor.base_rate_per_hour(),
            "education": Education.base_rate_per_hour(),
            "mentorship": Mentorship.base_rate_per_hour(),
            "care": Care.base_rate_per_hour(),
            "emergency_response": EmergencyResponse.base_rate_per_hour(),
            "rehabilitation": Rehabilitation.base_rate_per_hour(),
            "content_creation": ContentCreation.base_rate_per_hour(),
        }
    }))
}

pub fn routes() -> Routes {
    Routes::new()
        .prefix("api/nexus")
        .add("/parameters", get(parameters))
        .add("/nodes", get(list_nodes))
        .add("/nodes", post(create_node))
        .add("/nodes/{node_id}", get(get_node))
        .add("/nodes/{node_id}/wallet", get(get_wallet))
        .add("/nodes/{node_id}/wallet/sweep", post(sweep_wallet))
        .add("/nodes/{node_id}/standing", get(get_standing))
        .add("/contributions", post(record_contribution))
        .add("/demiurge/redeem", post(redeem))
        .add("/payments", get(list_payments))
        .add("/payments", post(process_payment))
        .add("/tokens", post(issue_token))
}
