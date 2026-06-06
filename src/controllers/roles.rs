#![allow(clippy::missing_errors_doc)]
#![allow(clippy::unnecessary_struct_initialization)]
#![allow(clippy::unused_async)]
use loco_rs::prelude::*;
use serde::{Deserialize, Serialize};

use crate::middleware::rbac;
use crate::models::roles;

#[derive(Debug, Deserialize)]
pub struct CreateRoleRequest {
    pub name: String,
    pub description: Option<String>,
    pub permissions: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct AssignRoleRequest {
    pub user_email: String,
    pub role_name: String,
}

#[derive(Debug, Serialize)]
pub struct RoleResponse {
    pub id: i32,
    pub name: String,
    pub description: Option<String>,
    pub permissions: Vec<String>,
    pub is_system: bool,
}

impl From<roles::Model> for RoleResponse {
    fn from(role: roles::Model) -> Self {
        Self {
            id: role.id,
            name: role.name.clone(),
            description: role.description,
            permissions: role.get_permissions(),
            is_system: role.is_system,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct UserRolesResponse {
    pub user_email: String,
    pub roles: Vec<String>,
}

/// List all roles
pub async fn list(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "roles:read").await?;

    let all_roles = roles::Model::find_all(&ctx.db).await?;
    let response: Vec<RoleResponse> = all_roles.into_iter().map(RoleResponse::from).collect();
    format::json(response)
}

/// Create a new role (admin only)
pub async fn create(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
    Json(req): Json<CreateRoleRequest>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "roles:create").await?;

    let params = roles::CreateRoleParams {
        name: req.name,
        description: req.description,
        permissions: req.permissions,
    };

    let role = roles::Model::create(&ctx.db, &params).await?;
    format::json(RoleResponse::from(role))
}

/// Assign a role to a user (admin only)
pub async fn assign(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
    Json(req): Json<AssignRoleRequest>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "roles:assign").await?;

    // Find the target user
    let target_user =
        crate::models::users::Model::find_by_email(&ctx.db, &req.user_email).await?;

    // Assign the role
    rbac::assign_role(&ctx.db, target_user.id, &req.role_name).await?;

    // Get the updated roles
    let user_roles = rbac::get_user_roles(&ctx.db, target_user.id).await?;

    format::json(UserRolesResponse {
        user_email: req.user_email,
        roles: user_roles,
    })
}

/// Get roles for the current user
pub async fn my_roles(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    let user_roles = rbac::get_user_roles(&ctx.db, user.id).await?;

    format::json(UserRolesResponse {
        user_email: user.email.clone(),
        roles: user_roles,
    })
}

/// Seed default system roles
pub async fn seed_defaults(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
) -> Result<Response> {
    let user = crate::models::users::Model::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    rbac::require_permission(&ctx.db, user.id, "roles:create").await?;

    roles::Model::seed_defaults(&ctx.db).await?;
    format::json(serde_json::json!({"message": "Default roles seeded successfully"}))
}

pub fn routes() -> Routes {
    Routes::new()
        .prefix("api/roles")
        .add("/", get(list))
        .add("/", post(create))
        .add("/assign", post(assign))
        .add("/my-roles", get(my_roles))
        .add("/seed-defaults", post(seed_defaults))
}
