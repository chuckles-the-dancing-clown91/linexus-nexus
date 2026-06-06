//! # RBAC Authorization Guard
//!
//! Role-based access control middleware for the Linexus Nexus.
//! Checks user permissions against role definitions stored in the database.
//!
//! ## Architecture
//! ```text
//! Request → JWT Auth (Loco built-in) → Extract User → RBAC Check → Controller
//! ```
//!
//! Roles are stored in the `roles` table and linked to users via `user_roles`.
//! Each role has a JSON array of permission strings supporting wildcards:
//! - `"*"` — grants all permissions
//! - `"tasks:*"` — grants all task-related permissions
//! - `"tasks:create"` — grants specific task creation permission

use crate::models::_entities::{roles, user_roles};
use loco_rs::prelude::*;

/// Check if a user (by their DB id) has the specified permission.
///
/// Looks up all roles assigned to the user, then checks each role's
/// permission list for a match (exact or wildcard).
pub async fn check_permission(
    db: &DatabaseConnection,
    user_id: i32,
    permission: &str,
) -> Result<bool> {
    // Get all role_ids for this user
    let assignments: Vec<user_roles::Model> = user_roles::Entity::find()
        .filter(
            model::query::condition()
                .eq(user_roles::Column::UserId, user_id)
                .build(),
        )
        .all(db)
        .await
        .map_err(|e| loco_rs::Error::Any(e.into()))?;

    if assignments.is_empty() {
        return Ok(false);
    }

    let role_ids: Vec<i32> = assignments.iter().map(|a| a.role_id).collect();

    // Get all matching roles
    let matched_roles: Vec<roles::Model> = roles::Entity::find()
        .filter(roles::Column::Id.is_in(role_ids))
        .all(db)
        .await
        .map_err(|e| loco_rs::Error::Any(e.into()))?;

    // Check if any role grants this permission
    for role in &matched_roles {
        let perms: Vec<String> = serde_json::from_str(&role.permissions).unwrap_or_default();
        for p in &perms {
            if p == "*" || p == permission {
                return Ok(true);
            }
            // Wildcard match: "tasks:*" matches "tasks:create"
            if p.ends_with('*') {
                let prefix = p.trim_end_matches('*');
                if permission.starts_with(prefix) {
                    return Ok(true);
                }
            }
        }
    }

    Ok(false)
}

/// Require a specific permission, returning an error if denied.
///
/// Use this in controllers to guard endpoints:
/// ```rust,ignore
/// rbac::require_permission(&ctx.db, user.id, "tasks:create").await?;
/// ```
pub async fn require_permission(
    db: &DatabaseConnection,
    user_id: i32,
    permission: &str,
) -> Result<()> {
    if check_permission(db, user_id, permission).await? {
        Ok(())
    } else {
        Err(loco_rs::Error::Unauthorized(format!(
            "Permission denied: requires '{}'",
            permission
        )))
    }
}

/// Assign a role to a user by role name.
pub async fn assign_role(
    db: &DatabaseConnection,
    user_id: i32,
    role_name: &str,
) -> Result<()> {
    let role = roles::Entity::find()
        .filter(
            model::query::condition()
                .eq(roles::Column::Name, role_name)
                .build(),
        )
        .one(db)
        .await
        .map_err(|e| loco_rs::Error::Any(e.into()))?
        .ok_or_else(|| loco_rs::Error::NotFound)?;

    // Check if assignment already exists
    let existing = user_roles::Entity::find()
        .filter(
            model::query::condition()
                .eq(user_roles::Column::UserId, user_id)
                .eq(user_roles::Column::RoleId, role.id)
                .build(),
        )
        .one(db)
        .await
        .map_err(|e| loco_rs::Error::Any(e.into()))?;

    if existing.is_none() {
        user_roles::ActiveModel {
            user_id: sea_orm::ActiveValue::set(user_id),
            role_id: sea_orm::ActiveValue::set(role.id),
            ..Default::default()
        }
        .insert(db)
        .await
        .map_err(|e| loco_rs::Error::Any(e.into()))?;
    }

    Ok(())
}

/// Get all role names for a user.
pub async fn get_user_roles(
    db: &DatabaseConnection,
    user_id: i32,
) -> Result<Vec<String>> {
    let assignments: Vec<user_roles::Model> = user_roles::Entity::find()
        .filter(
            model::query::condition()
                .eq(user_roles::Column::UserId, user_id)
                .build(),
        )
        .all(db)
        .await
        .map_err(|e| loco_rs::Error::Any(e.into()))?;

    let role_ids: Vec<i32> = assignments.iter().map(|a| a.role_id).collect();

    if role_ids.is_empty() {
        return Ok(vec![]);
    }

    let matched_roles: Vec<roles::Model> = roles::Entity::find()
        .filter(roles::Column::Id.is_in(role_ids))
        .all(db)
        .await
        .map_err(|e| loco_rs::Error::Any(e.into()))?;

    Ok(matched_roles.into_iter().map(|r| r.name).collect())
}
