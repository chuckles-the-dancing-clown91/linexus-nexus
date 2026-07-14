//! Housing occupancies — the assignment record linking a resident node to a
//! housing unit over time.
//!
//! `vacated_at = NULL` means the resident is currently occupying the unit.
//! [`Model::assign`] and [`Model::vacate`] are the only two write paths;
//! `vacate` also transitions the unit back to `make_ready` and re-enqueues it.

use loco_rs::prelude::*;
use sea_orm::{ActiveValue, QueryOrder, TransactionTrait};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use super::_entities::housing_occupancies::{self, ActiveModel, Entity, Model};
use super::{housing_queue, housing_units};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AssignParams {
    pub housing_unit_id: i32,
    pub resident_node_id: i32,
    #[serde(default)]
    pub assigned_by_node_id: Option<i32>,
    #[serde(default)]
    pub notes: Option<String>,
}

impl Model {
    /// The current (non-vacated) occupancy for a unit, if any.
    pub async fn current_for_unit<C: ConnectionTrait>(
        db: &C,
        housing_unit_id: i32,
    ) -> ModelResult<Option<Self>> {
        Ok(housing_occupancies::Entity::find()
            .filter(
                model::query::condition()
                    .eq(housing_occupancies::Column::HousingUnitId, housing_unit_id)
                    .is_null(housing_occupancies::Column::VacatedAt)
                    .build(),
            )
            .one(db)
            .await?)
    }

    /// All occupancies (current and historical) for a unit.
    pub async fn history_for_unit(
        db: &DatabaseConnection,
        housing_unit_id: i32,
    ) -> ModelResult<Vec<Self>> {
        Ok(housing_occupancies::Entity::find()
            .filter(
                model::query::condition()
                    .eq(housing_occupancies::Column::HousingUnitId, housing_unit_id)
                    .build(),
            )
            .order_by_desc(housing_occupancies::Column::AssignedAt)
            .all(db)
            .await?)
    }

    /// All active occupancies for a given resident node.
    pub async fn active_for_resident(
        db: &DatabaseConnection,
        resident_node_id: i32,
    ) -> ModelResult<Vec<Self>> {
        Ok(housing_occupancies::Entity::find()
            .filter(
                model::query::condition()
                    .eq(housing_occupancies::Column::ResidentNodeId, resident_node_id)
                    .is_null(housing_occupancies::Column::VacatedAt)
                    .build(),
            )
            .all(db)
            .await?)
    }

    /// Assign a resident to a unit.  Fails if the unit is already occupied.
    /// Atomically:
    ///   1. Creates the occupancy record.
    ///   2. Sets the unit status to `occupied`.
    ///   3. Marks the queue entry as `assigned`.
    pub async fn assign(db: &DatabaseConnection, params: &AssignParams) -> ModelResult<Self> {
        let txn = db.begin().await?;

        // Guard: unit must not already be occupied.
        if Self::current_for_unit(&txn, params.housing_unit_id)
            .await?
            .is_some()
        {
            txn.rollback().await?;
            return Err(ModelError::msg("unit is already occupied"));
        }

        let now: chrono::DateTime<chrono::FixedOffset> =
            chrono::Utc::now().fixed_offset().into();

        let occupancy = housing_occupancies::ActiveModel {
            occupancy_id: ActiveValue::set(Uuid::new_v4()),
            housing_unit_id: ActiveValue::set(params.housing_unit_id),
            resident_node_id: ActiveValue::set(params.resident_node_id),
            assigned_by_node_id: ActiveValue::set(params.assigned_by_node_id),
            assigned_at: ActiveValue::set(now),
            vacated_at: ActiveValue::set(None),
            notes: ActiveValue::set(params.notes.clone()),
            ..Default::default()
        }
        .insert(&txn)
        .await?;

        // Transition unit to occupied.
        housing_units::Model::set_status(&txn, params.housing_unit_id, "occupied").await?;

        // Close the queue entry for this unit.
        housing_queue::Model::mark_assigned(
            &txn,
            params.housing_unit_id,
            params.resident_node_id,
        )
        .await?;

        txn.commit().await?;
        Ok(occupancy)
    }

    /// Vacate a unit.  Atomically:
    ///   1. Stamps `vacated_at` on the current occupancy.
    ///   2. Sets the unit to `make_ready`.
    ///   3. Re-enqueues the unit as `available`.
    pub async fn vacate(
        db: &DatabaseConnection,
        housing_unit_id: i32,
        notes: Option<String>,
    ) -> ModelResult<Self> {
        let txn = db.begin().await?;

        let occupancy = Self::current_for_unit(&txn, housing_unit_id)
            .await?
            .ok_or_else(|| ModelError::msg("unit is not currently occupied"))?;

        let now: chrono::DateTime<chrono::FixedOffset> =
            chrono::Utc::now().fixed_offset().into();

        let mut active: housing_occupancies::ActiveModel = occupancy.into();
        if let Some(n) = notes {
            active.notes = ActiveValue::set(Some(n));
        }
        active.vacated_at = ActiveValue::set(Some(now));
        let updated = active.update(&txn).await?;

        // Transition unit to make_ready.
        housing_units::Model::set_status(&txn, housing_unit_id, "make_ready").await?;

        // Get the unit for its label.
        let unit = housing_units::Model::find_by_id(&txn, housing_unit_id).await?;

        // Re-enqueue the unit.
        housing_queue::Model::enqueue(&txn, housing_unit_id, &unit.unit_number, 0).await?;

        txn.commit().await?;
        Ok(updated)
    }
}
