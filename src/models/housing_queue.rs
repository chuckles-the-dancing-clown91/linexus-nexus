use loco_rs::prelude::*;
use sea_orm::{ActiveValue, ConnectionTrait, PaginatorTrait, QueryOrder};
use uuid::Uuid;

pub use super::_entities::housing_queue::{self, ActiveModel, Entity, Model};

impl Model {
    /// All available queue entries, ordered by priority desc then queued_at asc
    /// (FIFO within a priority band).
    pub async fn available<C: ConnectionTrait>(db: &C) -> ModelResult<Vec<Self>> {
        Ok(housing_queue::Entity::find()
            .filter(
                model::query::condition()
                    .eq(housing_queue::Column::Status, "available")
                    .build(),
            )
            .order_by_desc(housing_queue::Column::Priority)
            .order_by_asc(housing_queue::Column::QueuedAt)
            .all(db)
            .await?)
    }

    /// Current queue entry for a unit (any status), if any.
    pub async fn find_for_unit<C: ConnectionTrait>(
        db: &C,
        housing_unit_id: i32,
    ) -> ModelResult<Option<Self>> {
        Ok(housing_queue::Entity::find()
            .filter(
                model::query::condition()
                    .eq(housing_queue::Column::HousingUnitId, housing_unit_id)
                    .eq(housing_queue::Column::Status, "available")
                    .build(),
            )
            .one(db)
            .await?)
    }

    /// Add a unit to the queue with `available` status.  Safe to call
    /// multiple times — if the unit already has an `available` entry it is
    /// returned unchanged (idempotent).
    pub async fn enqueue<C: ConnectionTrait>(
        db: &C,
        housing_unit_id: i32,
        unit_label: &str,
        priority: i32,
    ) -> ModelResult<Self> {
        if let Some(existing) = Self::find_for_unit(db, housing_unit_id).await? {
            return Ok(existing);
        }

        let now: chrono::DateTime<chrono::FixedOffset> = chrono::Utc::now().fixed_offset();

        let entry = housing_queue::ActiveModel {
            queue_id: ActiveValue::set(Uuid::new_v4()),
            housing_unit_id: ActiveValue::set(housing_unit_id),
            unit_label: ActiveValue::set(unit_label.to_string()),
            status: ActiveValue::set("available".to_string()),
            queued_at: ActiveValue::set(now),
            claimed_at: ActiveValue::set(None),
            claimed_by_node_id: ActiveValue::set(None),
            priority: ActiveValue::set(priority),
            ..Default::default()
        }
        .insert(db)
        .await?;

        Ok(entry)
    }

    /// Mark the `available` queue entry for a unit as `assigned`, recording
    /// the resident node and claim timestamp.
    pub async fn mark_assigned<C: ConnectionTrait>(
        db: &C,
        housing_unit_id: i32,
        resident_node_id: i32,
    ) -> ModelResult<()> {
        let entry = Self::find_for_unit(db, housing_unit_id).await?;
        if let Some(entry) = entry {
            let now: chrono::DateTime<chrono::FixedOffset> = chrono::Utc::now().fixed_offset();
            let mut active: housing_queue::ActiveModel = entry.into();
            active.status = ActiveValue::set("assigned".to_string());
            active.claimed_at = ActiveValue::set(Some(now));
            active.claimed_by_node_id = ActiveValue::set(Some(resident_node_id));
            active.update(db).await?;
        }
        Ok(())
    }

    /// Total count of available queue entries.
    pub async fn available_count<C: ConnectionTrait>(db: &C) -> ModelResult<u64> {
        Ok(housing_queue::Entity::find()
            .filter(
                model::query::condition()
                    .eq(housing_queue::Column::Status, "available")
                    .build(),
            )
            .count(db)
            .await?)
    }
}
