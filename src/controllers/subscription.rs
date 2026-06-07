#![allow(clippy::missing_errors_doc)]
#![allow(clippy::unnecessary_struct_initialization)]
#![allow(clippy::unused_async)]
//! # Subscription Controller
//!
//! Self-service subscription plan management. Plans are an entitlement tier
//! (free / pro / enterprise) — there is **no payment**; changing plan is an
//! immediate entitlement switch that also re-points the user's baseline RBAC
//! role. See `models::plans` for the catalog and `middleware::rbac` for how
//! plan permissions fold into authorization.

use loco_rs::prelude::*;
use serde::{Deserialize, Serialize};

use crate::middleware::rbac;
use crate::models::{plans, users};

#[derive(Debug, Deserialize)]
pub struct ChangePlanRequest {
    pub plan: String,
}

#[derive(Debug, Serialize)]
pub struct PlanView {
    pub id: String,
    pub label: String,
    pub description: String,
    pub default_role: String,
    pub permissions: Vec<String>,
}

impl From<plans::PlanInfo> for PlanView {
    fn from(p: plans::PlanInfo) -> Self {
        Self {
            id: p.id.to_string(),
            label: p.label.to_string(),
            description: p.description.to_string(),
            default_role: p.default_role.to_string(),
            permissions: p.permissions.iter().map(|s| (*s).to_string()).collect(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SubscriptionResponse {
    /// The user's current plan id.
    pub plan: String,
    /// Effective permissions the user holds (role ∪ plan).
    pub permissions: Vec<String>,
    /// RBAC roles currently assigned to the user.
    pub roles: Vec<String>,
    /// The full plan catalog the user can switch between.
    pub available_plans: Vec<PlanView>,
}

async fn build_response(ctx: &AppContext, user: &users::Model) -> Result<SubscriptionResponse> {
    let roles = rbac::get_user_roles(&ctx.db, user.id).await?;
    let permissions = rbac::get_effective_permissions(&ctx.db, user.id).await?;
    Ok(SubscriptionResponse {
        plan: user.plan.clone(),
        permissions,
        roles,
        available_plans: plans::catalog().into_iter().map(PlanView::from).collect(),
    })
}

/// Get the current user's subscription state and the available plan catalog.
pub async fn show(auth: auth::JWT, State(ctx): State<AppContext>) -> Result<Response> {
    let user = users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    format::json(build_response(&ctx, &user).await?)
}

/// Change the current user's subscription plan (self-service, no payment).
///
/// Switching plan re-assigns the baseline role for the target plan so the
/// user's roles stay coherent with their tier. Previously-assigned roles are
/// preserved (a downgrade narrows plan-granted permissions but does not strip
/// explicitly-assigned roles).
pub async fn change(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
    Json(req): Json<ChangePlanRequest>,
) -> Result<Response> {
    let user = users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;

    if !plans::is_valid(&req.plan) {
        return bad_request(format!("unknown plan '{}'", req.plan));
    }

    let user = user
        .into_active_model()
        .set_plan(&ctx.db, &req.plan)
        .await?;

    // Keep the baseline role coherent with the new plan. Best-effort: a missing
    // role must not fail the plan change.
    let default_role = plans::default_role(&user.plan);
    if let Err(err) = rbac::assign_role(&ctx.db, user.id, default_role).await {
        tracing::warn!(
            user_pid = user.pid.to_string(),
            role = default_role,
            error = err.to_string(),
            "could not assign baseline role after plan change",
        );
    }

    format::json(build_response(&ctx, &user).await?)
}

pub fn routes() -> Routes {
    Routes::new()
        .prefix("/api/subscription")
        .add("/", get(show))
        .add("/", post(change))
}
