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

use crate::models::_entities::{roles, user_roles, users};
use crate::models::plans;
use loco_rs::prelude::*;

/// Returns true if `permission` is matched by any pattern in `granted`,
/// honoring `"*"` (all) and trailing-`*` (prefix) wildcards.
fn matches_any(granted: &[String], permission: &str) -> bool {
    granted.iter().any(|p| {
        p == "*"
            || p == permission
            || (p.ends_with('*') && permission.starts_with(p.trim_end_matches('*')))
    })
}

/// Check if a user (by their DB id) has the specified permission.
///
/// A user is granted a permission if **either** an assigned role **or** their
/// current subscription plan grants it. Both axes support `"*"` and trailing-`*`
/// wildcards.
pub async fn check_permission(
    db: &DatabaseConnection,
    user_id: i32,
    permission: &str,
) -> Result<bool> {
    let granted = get_effective_permissions(db, user_id).await?;
    Ok(matches_any(&granted, permission))
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
pub async fn assign_role(db: &DatabaseConnection, user_id: i32, role_name: &str) -> Result<()> {
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
pub async fn get_user_roles(db: &DatabaseConnection, user_id: i32) -> Result<Vec<String>> {
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

/// Get a user's current subscription plan id (e.g. `"free"`).
pub async fn get_user_plan(db: &DatabaseConnection, user_id: i32) -> Result<String> {
    let user = users::Entity::find_by_id(user_id)
        .one(db)
        .await
        .map_err(|e| loco_rs::Error::Any(e.into()))?
        .ok_or_else(|| loco_rs::Error::NotFound)?;
    Ok(user.plan)
}

/// Compute the full set of permission patterns a user effectively holds: the
/// union of every assigned role's permissions and the permissions granted by
/// their current subscription plan. Returned patterns may include wildcards.
pub async fn get_effective_permissions(
    db: &DatabaseConnection,
    user_id: i32,
) -> Result<Vec<String>> {
    let mut granted: Vec<String> = Vec::new();

    // Role-derived permissions.
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
    if !role_ids.is_empty() {
        let matched_roles: Vec<roles::Model> = roles::Entity::find()
            .filter(roles::Column::Id.is_in(role_ids))
            .all(db)
            .await
            .map_err(|e| loco_rs::Error::Any(e.into()))?;
        for role in &matched_roles {
            let perms: Vec<String> = serde_json::from_str(&role.permissions).unwrap_or_default();
            granted.extend(perms);
        }
    }

    // Plan-derived permissions.
    let plan = get_user_plan(db, user_id).await?;
    granted.extend(plans::permissions(&plan));

    granted.sort();
    granted.dedup();
    Ok(granted)
}
