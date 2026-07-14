use loco_rs::prelude::*;
use sea_orm::ActiveValue;
use serde::{Deserialize, Serialize};

pub use super::_entities::roles::{self, ActiveModel, Entity, Model};

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateRoleParams {
    pub name: String,
    pub description: Option<String>,
    pub permissions: Vec<String>,
}

impl Model {
    /// Find a role by name
    pub async fn find_by_name(db: &DatabaseConnection, name: &str) -> ModelResult<Self> {
        let role = roles::Entity::find()
            .filter(
                model::query::condition()
                    .eq(roles::Column::Name, name)
                    .build(),
            )
            .one(db)
            .await?;
        role.ok_or_else(|| ModelError::EntityNotFound)
    }

    /// Get all roles
    pub async fn find_all(db: &DatabaseConnection) -> ModelResult<Vec<Self>> {
        Ok(roles::Entity::find().all(db).await?)
    }

    /// Create a new role
    pub async fn create(db: &DatabaseConnection, params: &CreateRoleParams) -> ModelResult<Self> {
        let permissions_json =
            serde_json::to_string(&params.permissions).map_err(|e| ModelError::Any(e.into()))?;

        let role = roles::ActiveModel {
            name: ActiveValue::set(params.name.clone()),
            description: ActiveValue::set(params.description.clone()),
            permissions: ActiveValue::set(permissions_json),
            is_system: ActiveValue::set(false),
            ..Default::default()
        }
        .insert(db)
        .await?;

        Ok(role)
    }

    /// Get parsed permissions
    pub fn get_permissions(&self) -> Vec<String> {
        serde_json::from_str(&self.permissions).unwrap_or_default()
    }

    /// Check if this role grants a specific permission
    pub fn has_permission(&self, permission: &str) -> bool {
        let perms = self.get_permissions();
        perms.iter().any(|p| {
            p == permission
                || p == "*"
                || (p.ends_with("*") && permission.starts_with(p.trim_end_matches('*')))
        })
    }

    /// Seed default system roles
    pub async fn seed_defaults(db: &DatabaseConnection) -> ModelResult<()> {
        let defaults = vec![
            ("admin", "Full system administrator", vec!["*".to_string()]),
            (
                "operator",
                "Can create and manage tasks and agents",
                vec![
                    "tasks:create".to_string(),
                    "tasks:read".to_string(),
                    "tasks:cancel".to_string(),
                    "agents:read".to_string(),
                    "agents:enroll".to_string(),
                    "roles:read".to_string(),
                ],
            ),
            (
                "viewer",
                "Read-only access to tasks and agents",
                vec![
                    "tasks:read".to_string(),
                    "agents:read".to_string(),
                    "roles:read".to_string(),
                ],
            ),
            (
                "auditor",
                "Read access with audit log visibility",
                vec![
                    "tasks:read".to_string(),
                    "agents:read".to_string(),
                    "roles:read".to_string(),
                    "logs:read".to_string(),
                    "audit:read".to_string(),
                ],
            ),
            (
                "housing_manager",
                "Manages housing nodes, units, maintenance and docs",
                vec![
                    "housing:read".to_string(),
                    "housing:write".to_string(),
                ],
            ),
            (
                "council",
                "Civic authority over housing activation, reviews and resident assignment",
                vec![
                    "housing:read".to_string(),
                    "housing:review".to_string(),
                    "housing:assign".to_string(),
                    "housing:vacate".to_string(),
                ],
            ),
        ];

        for (name, desc, perms) in defaults {
            if roles::Entity::find()
                .filter(
                    model::query::condition()
                        .eq(roles::Column::Name, name)
                        .build(),
                )
                .one(db)
                .await?
                .is_none()
            {
                let permissions_json =
                    serde_json::to_string(&perms).map_err(|e| ModelError::Any(e.into()))?;
                roles::ActiveModel {
                    name: ActiveValue::set(name.to_string()),
                    description: ActiveValue::set(Some(desc.to_string())),
                    permissions: ActiveValue::set(permissions_json),
                    is_system: ActiveValue::set(true),
                    ..Default::default()
                }
                .insert(db)
                .await?;
            }
        }

        Ok(())
    }
}
