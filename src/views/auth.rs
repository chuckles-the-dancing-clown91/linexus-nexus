use serde::{Deserialize, Serialize};

use crate::models::_entities::users;

#[derive(Debug, Deserialize, Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub pid: String,
    pub name: String,
    pub is_verified: bool,
    /// Subscription plan tier (e.g. `free`, `pro`, `enterprise`).
    pub plan: String,
    /// RBAC role names assigned to the user.
    pub roles: Vec<String>,
    /// Effective permission patterns (role ∪ plan), for client-side gating.
    pub permissions: Vec<String>,
}

impl LoginResponse {
    #[must_use]
    pub fn new(
        user: &users::Model,
        token: &String,
        roles: Vec<String>,
        permissions: Vec<String>,
    ) -> Self {
        Self {
            token: token.to_string(),
            pid: user.pid.to_string(),
            name: user.name.clone(),
            is_verified: user.email_verified_at.is_some(),
            plan: user.plan.clone(),
            roles,
            permissions,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CurrentResponse {
    pub pid: String,
    pub name: String,
    pub email: String,
    /// Subscription plan tier (e.g. `free`, `pro`, `enterprise`).
    pub plan: String,
    /// RBAC role names assigned to the user.
    pub roles: Vec<String>,
    /// Effective permission patterns (role ∪ plan), for client-side gating.
    pub permissions: Vec<String>,
}

impl CurrentResponse {
    #[must_use]
    pub fn new(user: &users::Model, roles: Vec<String>, permissions: Vec<String>) -> Self {
        Self {
            pid: user.pid.to_string(),
            name: user.name.clone(),
            email: user.email.clone(),
            plan: user.plan.clone(),
            roles,
            permissions,
        }
    }
}
