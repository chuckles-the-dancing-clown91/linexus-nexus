//! # Permission Funnel — Multi-Tier Authorization
//!
//! Implements the Linexus "Permission Funnel" concept: a layered authorization
//! pipeline that evaluates access through multiple tiers before allowing an operation.
//!
//! ## Tier Architecture
//!
//! ```text
//! Request
//!   ├── Tier 1: Global Deny List (blocked users/IPs)
//!   ├── Tier 2: RBAC Role Check (does user have the required role/permission?)
//!   ├── Tier 3: Resource Policy (can user X manage resource Y?)
//!   └── Tier 4: Step-Up Auth (destructive operations require re-authentication)
//! ```
//!
//! Each tier can DENY the request. If all tiers pass, the request proceeds.

use crate::middleware::rbac;
use loco_rs::prelude::*;
use serde::{Deserialize, Serialize};

/// The result of evaluating the Permission Funnel.
#[derive(Debug, Serialize)]
pub struct FunnelResult {
    pub allowed: bool,
    pub denied_at_tier: Option<u8>,
    pub reason: Option<String>,
}

/// Represents the intent of a system operation for funnel evaluation.
#[derive(Debug, Deserialize, Serialize)]
pub struct OperationIntent {
    /// The permission required (e.g., "tasks:create")
    pub permission: String,
    /// Whether this is a destructive operation
    pub is_destructive: bool,
    /// Optional resource identifier for tier-3 checks
    pub resource_id: Option<String>,
}

impl OperationIntent {
    pub fn new(permission: &str) -> Self {
        let is_destructive = Self::detect_destructive(permission);
        Self {
            permission: permission.to_string(),
            is_destructive,
            resource_id: None,
        }
    }

    pub fn with_resource(mut self, resource_id: &str) -> Self {
        self.resource_id = Some(resource_id.to_string());
        self
    }

    fn detect_destructive(permission: &str) -> bool {
        let destructive_actions = [
            "delete",
            "remove",
            "destroy",
            "format",
            "partition",
            "wipe",
            "cancel",
        ];
        let perm_lower = permission.to_lowercase();
        destructive_actions
            .iter()
            .any(|action| perm_lower.contains(action))
    }
}

/// Evaluate the full permission funnel for an operation.
///
/// Returns a `FunnelResult` indicating whether the operation is allowed
/// and which tier (if any) denied it.
pub async fn evaluate(
    db: &DatabaseConnection,
    user_id: i32,
    intent: &OperationIntent,
    step_up_token: Option<&str>,
) -> Result<FunnelResult> {
    // === Tier 1: Global Deny List ===
    // TODO: Check against a deny list (blocked users, IP restrictions, etc.)
    // For now, no users are globally denied.

    // === Tier 2: RBAC Role Check ===
    let has_permission = rbac::check_permission(db, user_id, &intent.permission).await?;
    if !has_permission {
        return Ok(FunnelResult {
            allowed: false,
            denied_at_tier: Some(2),
            reason: Some(format!(
                "RBAC: user lacks permission '{}'",
                intent.permission
            )),
        });
    }

    // === Tier 3: Resource Policy ===
    // TODO: Check resource-level policies (e.g., "can user X manage hostgroup Y?")
    // For now, if the RBAC check passes, resource-level access is granted.

    // === Tier 4: Step-Up Authentication ===
    if intent.is_destructive {
        if step_up_token.is_none() {
            return Ok(FunnelResult {
                allowed: false,
                denied_at_tier: Some(4),
                reason: Some(
                    "Step-up authentication required for destructive operations".to_string(),
                ),
            });
        }
        // TODO: Verify the step-up token against the auth system.
        // For now, any non-empty token is accepted.
    }

    Ok(FunnelResult {
        allowed: true,
        denied_at_tier: None,
        reason: None,
    })
}
