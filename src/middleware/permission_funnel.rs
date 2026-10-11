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
//!
//! ## Step-up is not yet available
//!
//! Nothing in Nexus (or the Hub) mints a step-up token, so there is nothing a
//! presented token can be verified against. Until a verifier exists, tier 4
//! **refuses every destructive operation**, token or not — see
//! [`step_up_refusal`]. Earlier versions accepted any non-empty string, which
//! made the tier decorative. Wiring a real verifier means replacing that
//! function's body, nothing else.

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

/// Tier 4 without I/O: why a destructive operation is refused, or `None`
/// when the operation is not destructive. No step-up verifier exists yet, so
/// a presented token is refused as unverifiable rather than taken on faith.
#[must_use]
pub fn step_up_refusal(is_destructive: bool, step_up_token: Option<&str>) -> Option<&'static str> {
    if !is_destructive {
        return None;
    }
    match step_up_token.map(str::trim) {
        Some(t) if !t.is_empty() => Some(
            "step-up token cannot be verified: no step-up verifier is configured, so destructive operations are refused",
        ),
        _ => Some("Step-up authentication required for destructive operations"),
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
    // Destructive operations require a verified step-up token, and no
    // verifier exists yet (module docs), so they are refused outright.
    if let Some(reason) = step_up_refusal(intent.is_destructive, step_up_token) {
        return Ok(FunnelResult {
            allowed: false,
            denied_at_tier: Some(4),
            reason: Some(reason.to_string()),
        });
    }

    Ok(FunnelResult {
        allowed: true,
        denied_at_tier: None,
        reason: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destructive_operations_are_refused_until_a_verifier_exists() {
        assert_eq!(step_up_refusal(false, None), None);
        assert_eq!(step_up_refusal(false, Some("anything")), None);
        assert!(step_up_refusal(true, None).is_some());
        assert!(step_up_refusal(true, Some("   ")).is_some());
        // The regression: a non-empty token used to pass.
        assert!(step_up_refusal(true, Some("totally-legit")).is_some());
        assert!(OperationIntent::new("tasks:delete").is_destructive);
        assert!(!OperationIntent::new("tasks:create").is_destructive);
    }
}
