use loco_rs::prelude::*;
use sea_orm::{ActiveValue, QueryOrder};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use super::_entities::nodes::{self, ActiveModel, Entity, Model};

/// Parameters for commissioning a node. When a trusted service (e.g. the Tea &
/// Madness publisher) creates an account, it commissions a `human` node here so
/// the account gains a wallet and a place in the supply/demand engine.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CommissionParams {
    /// human | infra_generator | infra_consumer | council
    pub class: String,
    pub label: String,
    #[serde(default)]
    pub lifecycle_phase: Option<String>,
    /// JSON-encoded array of capability strings.
    #[serde(default)]
    pub capabilities: Option<String>,
    #[serde(default)]
    pub public_key: Option<String>,
    #[serde(default)]
    pub owner_user_id: Option<i32>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub external_ref: Option<String>,
}

impl Model {
    /// Find a node by its public UUID.
    pub async fn find_by_node_id(db: &DatabaseConnection, node_id: &Uuid) -> ModelResult<Self> {
        nodes::Entity::find()
            .filter(
                model::query::condition()
                    .eq(nodes::Column::NodeId, *node_id)
                    .build(),
            )
            .one(db)
            .await?
            .ok_or_else(|| ModelError::EntityNotFound)
    }

    /// Find a node previously commissioned by `source` for `external_ref`.
    /// Used to make the "create account -> create node" push idempotent.
    pub async fn find_by_external(
        db: &DatabaseConnection,
        source: &str,
        external_ref: &str,
    ) -> ModelResult<Option<Self>> {
        Ok(nodes::Entity::find()
            .filter(
                model::query::condition()
                    .eq(nodes::Column::Source, source)
                    .eq(nodes::Column::ExternalRef, external_ref)
                    .build(),
            )
            .one(db)
            .await?)
    }

    /// Find the node owned by a given Nexus user, if any.
    pub async fn find_by_owner(
        db: &DatabaseConnection,
        owner_user_id: i32,
    ) -> ModelResult<Option<Self>> {
        Ok(nodes::Entity::find()
            .filter(
                model::query::condition()
                    .eq(nodes::Column::OwnerUserId, owner_user_id)
                    .build(),
            )
            .one(db)
            .await?)
    }

    /// List all nodes, newest first.
    pub async fn find_all(db: &DatabaseConnection) -> ModelResult<Vec<Self>> {
        Ok(nodes::Entity::find()
            .order_by_desc(nodes::Column::Id)
            .all(db)
            .await?)
    }

    /// Commission a node. If `source`+`external_ref` already map to a node, the
    /// existing node is returned unchanged so the operation is idempotent.
    pub async fn commission(
        db: &DatabaseConnection,
        params: &CommissionParams,
    ) -> ModelResult<Self> {
        if let (Some(src), Some(ext)) = (&params.source, &params.external_ref) {
            if let Some(existing) = Self::find_by_external(db, src, ext).await? {
                return Ok(existing);
            }
        }

        let node = nodes::ActiveModel {
            node_id: ActiveValue::set(Uuid::new_v4()),
            class: ActiveValue::set(params.class.clone()),
            label: ActiveValue::set(params.label.clone()),
            status: ActiveValue::set("active".to_string()),
            lifecycle_phase: ActiveValue::set(params.lifecycle_phase.clone()),
            capabilities: ActiveValue::set(params.capabilities.clone()),
            public_key: ActiveValue::set(params.public_key.clone()),
            owner_user_id: ActiveValue::set(params.owner_user_id),
            source: ActiveValue::set(params.source.clone()),
            external_ref: ActiveValue::set(params.external_ref.clone()),
            commissioned_at: ActiveValue::set(Some(chrono::Local::now().into())),
            ..Default::default()
        }
        .insert(db)
        .await?;

        Ok(node)
    }

    pub fn is_council(&self) -> bool {
        self.class == "council"
    }

    pub fn is_housing(&self) -> bool {
        self.class == "housing"
    }
}
