//! Housing node model — a managed property backed by a Linexus node.
//!
//! A housing node begins in `draft`, is submitted for council review
//! (`pending_council`), and becomes `active` once the quorum of council
//! approvals is met.  From `active` it can enter `vacating` (all units
//! released) before being `archived`.
//!
//! The `status` field is a state machine.  Use [`Model::transition`] to change
//! it — direct ActiveModel writes are intentionally left out of the business
//! layer.

use loco_rs::prelude::*;
use sea_orm::{ActiveValue, QueryOrder};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use super::_entities::housing_nodes::{self, ActiveModel, Entity, Model};

/// Allowed property types (mirrors Acre's taxonomy, filtered to Linexus uses).
pub const VALID_TYPES: &[&str] = &[
    "single_family",
    "multi_family",
    "condo",
    "townhome",
    "dormitory",
    "shelter",
];

// ── State machine ────────────────────────────────────────────────────────────

/// All possible statuses for a housing node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HousingStatus {
    Draft,
    PendingCouncil,
    Active,
    Vacating,
    Archived,
}

impl HousingStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::PendingCouncil => "pending_council",
            Self::Active => "active",
            Self::Vacating => "vacating",
            Self::Archived => "archived",
        }
    }

    // Inherent `from_str` returning Option (not the fallible FromStr trait).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "draft" => Some(Self::Draft),
            "pending_council" => Some(Self::PendingCouncil),
            "active" => Some(Self::Active),
            "vacating" => Some(Self::Vacating),
            "archived" => Some(Self::Archived),
            _ => None,
        }
    }

    /// Whether this transition is legal.
    pub fn can_transition_to(&self, next: &HousingStatus) -> bool {
        matches!(
            (self, next),
            (Self::Draft, Self::PendingCouncil)
                | (Self::PendingCouncil, Self::Active)
                | (Self::PendingCouncil, Self::Draft) // rejected → back to draft
                | (Self::Active, Self::Vacating)
                | (Self::Vacating, Self::Archived)
                | (Self::Vacating, Self::Active) // all units re-occupied
                | (Self::Active, Self::Archived)
        )
    }
}

// ── Params ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreateParams {
    /// The `nodes`.node_id UUID of the backing housing node (class=housing).
    pub node_id: Uuid,
    pub name: String,
    pub address: String,
    #[serde(default)]
    pub city: Option<String>,
    #[serde(default)]
    pub state_province: Option<String>,
    #[serde(default)]
    pub postal_code: Option<String>,
    pub property_type: String,
    pub unit_count: i32,
    #[serde(default = "default_quorum")]
    pub quorum_required: i32,
    #[serde(default)]
    pub notes: Option<String>,
}

fn default_quorum() -> i32 {
    2
}

// ── Model methods ────────────────────────────────────────────────────────────

impl Model {
    /// Find a housing node by its backing `node_id` UUID.
    pub async fn find_by_node_id(db: &DatabaseConnection, node_id: Uuid) -> ModelResult<Self> {
        housing_nodes::Entity::find()
            .filter(
                model::query::condition()
                    .eq(housing_nodes::Column::NodeId, node_id)
                    .build(),
            )
            .one(db)
            .await?
            .ok_or_else(|| ModelError::EntityNotFound)
    }

    /// Find a housing node by its primary key `id`.
    pub async fn find_by_id(db: &DatabaseConnection, id: i32) -> ModelResult<Self> {
        housing_nodes::Entity::find_by_id(id)
            .one(db)
            .await?
            .ok_or_else(|| ModelError::EntityNotFound)
    }

    /// List all housing nodes, newest first.
    pub async fn find_all(db: &DatabaseConnection) -> ModelResult<Vec<Self>> {
        Ok(housing_nodes::Entity::find()
            .order_by_desc(housing_nodes::Column::Id)
            .all(db)
            .await?)
    }

    /// List housing nodes by status.
    pub async fn find_by_status(db: &DatabaseConnection, status: &str) -> ModelResult<Vec<Self>> {
        Ok(housing_nodes::Entity::find()
            .filter(
                model::query::condition()
                    .eq(housing_nodes::Column::Status, status)
                    .build(),
            )
            .order_by_desc(housing_nodes::Column::Id)
            .all(db)
            .await?)
    }

    /// Create a new housing node in `draft` status.
    pub async fn create(db: &DatabaseConnection, params: &CreateParams) -> ModelResult<Self> {
        if !VALID_TYPES.contains(&params.property_type.as_str()) {
            return Err(ModelError::Message(format!(
                "invalid property_type '{}'; valid: {}",
                params.property_type,
                VALID_TYPES.join(", ")
            )));
        }
        if params.unit_count < 1 {
            return Err(ModelError::msg("unit_count must be >= 1"));
        }

        let node = housing_nodes::ActiveModel {
            node_id: ActiveValue::set(params.node_id),
            name: ActiveValue::set(params.name.clone()),
            address: ActiveValue::set(params.address.clone()),
            city: ActiveValue::set(params.city.clone()),
            state_province: ActiveValue::set(params.state_province.clone()),
            postal_code: ActiveValue::set(params.postal_code.clone()),
            property_type: ActiveValue::set(params.property_type.clone()),
            unit_count: ActiveValue::set(params.unit_count),
            status: ActiveValue::set(HousingStatus::Draft.as_str().to_string()),
            quorum_required: ActiveValue::set(params.quorum_required),
            notes: ActiveValue::set(params.notes.clone()),
            submitted_at: ActiveValue::set(None),
            activated_at: ActiveValue::set(None),
            ..Default::default()
        }
        .insert(db)
        .await?;

        Ok(node)
    }

    /// Transition a housing node to a new status.  Enforces the state machine.
    pub async fn transition(
        db: &DatabaseConnection,
        id: i32,
        next: HousingStatus,
    ) -> ModelResult<Self> {
        let node = Self::find_by_id(db, id).await?;
        let current = HousingStatus::from_str(&node.status)
            .ok_or_else(|| ModelError::msg("unknown current status"))?;

        if !current.can_transition_to(&next) {
            return Err(ModelError::Message(format!(
                "cannot transition housing node from '{}' to '{}'",
                current.as_str(),
                next.as_str()
            )));
        }

        let mut active: housing_nodes::ActiveModel = node.into();
        active.status = ActiveValue::set(next.as_str().to_string());

        match next {
            HousingStatus::PendingCouncil => {
                active.submitted_at = ActiveValue::set(Some(chrono::Utc::now().fixed_offset()));
            }
            HousingStatus::Active => {
                active.activated_at = ActiveValue::set(Some(chrono::Utc::now().fixed_offset()));
            }
            _ => {}
        }

        active.update(db).await.map_err(ModelError::from)
    }

    /// Check if the quorum has been met for `pending_council` nodes.
    /// If so, automatically transitions the node to `active` within a
    /// transaction and returns the updated model.
    pub async fn check_and_activate(db: &DatabaseConnection, id: i32) -> ModelResult<Option<Self>> {
        let node = Self::find_by_id(db, id).await?;
        if node.status != HousingStatus::PendingCouncil.as_str() {
            return Ok(None);
        }

        let approvals = super::housing_council_reviews::Model::count_approvals(db, id).await?;
        if approvals >= node.quorum_required as i64 {
            let activated = Self::transition(db, id, HousingStatus::Active).await?;
            // Council activation is what opens the commons pool: every
            // available unit enters the housing queue now, not at creation.
            let units = super::housing_units::Model::find_for_node(db, id).await?;
            for unit in &units {
                if unit.status == super::housing_units::UnitStatus::Available.as_str() {
                    super::housing_queue::Model::enqueue(db, unit.id, &unit.unit_number, 0).await?;
                }
            }
            Ok(Some(activated))
        } else {
            Ok(None)
        }
    }
}
