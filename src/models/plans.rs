//! # Subscription Plan Tiers
//!
//! Plans are a feature-gating axis layered on top of RBAC. Every user has a
//! `plan` (stored on the users row); each plan maps to a bundle of permissions
//! and a baseline role. There is **no payment** involved — switching plans is a
//! self-service entitlement change (see `controllers::subscription`).
//!
//! Effective permissions for a user are the union of:
//!   1. permissions granted by their assigned roles (`user_roles` → `roles`)
//!   2. permissions granted by their current plan (this module)
//!
//! ```text
//! free       → read-only console
//! pro        → operate: create/cancel tasks, enroll agents
//! enterprise → full administrative access
//! ```

use serde::Serialize;

/// The canonical default plan assigned to every new sign-up.
pub const DEFAULT_PLAN: &str = "free";

/// Static description of a plan tier, surfaced to the frontend so it can render
/// the subscription screen and gate features without hard-coding the catalog.
#[derive(Debug, Clone, Serialize)]
pub struct PlanInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    /// Baseline RBAC role assigned when a user is on this plan.
    pub default_role: &'static str,
    /// Permission bundle granted purely by holding this plan.
    pub permissions: Vec<&'static str>,
}

/// The full plan catalog, ordered cheapest → richest.
#[must_use]
pub fn catalog() -> Vec<PlanInfo> {
    vec![
        PlanInfo {
            id: "free",
            label: "Free",
            description: "Read-only access to tasks, agents and roles.",
            default_role: "viewer",
            permissions: vec!["tasks:read", "agents:read", "roles:read"],
        },
        PlanInfo {
            id: "pro",
            label: "Pro",
            description: "Operate the fleet: create and cancel tasks, enroll agents.",
            default_role: "operator",
            permissions: vec![
                "tasks:read",
                "tasks:create",
                "tasks:cancel",
                "agents:read",
                "agents:enroll",
                "roles:read",
            ],
        },
        PlanInfo {
            id: "enterprise",
            label: "Enterprise",
            description: "Full administrative access across every resource.",
            default_role: "admin",
            permissions: vec!["*"],
        },
    ]
}

/// Look up a plan by id.
#[must_use]
pub fn find(plan: &str) -> Option<PlanInfo> {
    catalog().into_iter().find(|p| p.id == plan)
}

/// Whether `plan` is a known plan id.
#[must_use]
pub fn is_valid(plan: &str) -> bool {
    catalog().iter().any(|p| p.id == plan)
}

/// The permission bundle granted by a plan (empty for unknown plans).
#[must_use]
pub fn permissions(plan: &str) -> Vec<String> {
    find(plan)
        .map(|p| p.permissions.iter().map(|s| (*s).to_string()).collect())
        .unwrap_or_default()
}

/// The baseline RBAC role for a plan (defaults to `viewer` for unknown plans).
#[must_use]
pub fn default_role(plan: &str) -> &'static str {
    find(plan).map_or("viewer", |p| p.default_role)
}

/// Whether a plan's bundle grants a specific permission (supports `*` and
/// trailing-`*` wildcards, matching the RBAC semantics).
#[must_use]
pub fn grants(plan: &str, permission: &str) -> bool {
    permissions(plan).iter().any(|p| {
        p == "*"
            || p == permission
            || (p.ends_with('*') && permission.starts_with(p.trim_end_matches('*')))
    })
}
