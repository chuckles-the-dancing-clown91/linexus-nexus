//! Housing maintenance model — tracks work orders against units.

use loco_rs::prelude::*;
use sea_orm::{ActiveValue, QueryOrder};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use super::_entities::housing_maintenance_tickets::{self, ActiveModel, Entity, Model};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreateTicketParams {
    pub housing_unit_id: i32,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    /// low | medium | high | emergency
    pub priority: String,
    #[serde(default)]
    pub opened_by_node_id: Option<i32>,
    #[serde(default)]
    pub assigned_to_node_id: Option<i32>,
}

impl Model {
    pub async fn find_by_id(db: &DatabaseConnection, id: i32) -> ModelResult<Self> {
        housing_maintenance_tickets::Entity::find_by_id(id)
            .one(db)
            .await?
            .ok_or_else(|| ModelError::EntityNotFound)
    }

    /// List all tickets for a specific housing unit.
    pub async fn find_for_unit(
        db: &DatabaseConnection,
        housing_unit_id: i32,
    ) -> ModelResult<Vec<Self>> {
        Ok(housing_maintenance_tickets::Entity::find()
            .filter(
                model::query::condition()
                    .eq(
                        housing_maintenance_tickets::Column::HousingUnitId,
                        housing_unit_id,
                    )
                    .build(),
            )
            .order_by_desc(housing_maintenance_tickets::Column::OpenedAt)
            .all(db)
            .await?)
    }

    /// List all open/in-progress maintenance tickets in the system.
    pub async fn find_active(db: &DatabaseConnection) -> ModelResult<Vec<Self>> {
        Ok(housing_maintenance_tickets::Entity::find()
            .filter(
                model::query::condition()
                    .ne(housing_maintenance_tickets::Column::Status, "closed")
                    .ne(housing_maintenance_tickets::Column::Status, "resolved")
                    .build(),
            )
            .order_by_desc(housing_maintenance_tickets::Column::OpenedAt)
            .all(db)
            .await?)
    }

    /// Create a maintenance ticket.
    pub async fn create(db: &DatabaseConnection, params: &CreateTicketParams) -> ModelResult<Self> {
        let now: chrono::DateTime<chrono::FixedOffset> = chrono::Utc::now().fixed_offset();

        let ticket = housing_maintenance_tickets::ActiveModel {
            ticket_id: ActiveValue::set(Uuid::new_v4()),
            housing_unit_id: ActiveValue::set(params.housing_unit_id),
            title: ActiveValue::set(params.title.clone()),
            description: ActiveValue::set(params.description.clone()),
            priority: ActiveValue::set(params.priority.clone()),
            status: ActiveValue::set("open".to_string()),
            opened_by_node_id: ActiveValue::set(params.opened_by_node_id),
            assigned_to_node_id: ActiveValue::set(params.assigned_to_node_id),
            opened_at: ActiveValue::set(now),
            resolved_at: ActiveValue::set(None),
            notes: ActiveValue::set(None),
            ..Default::default()
        }
        .insert(db)
        .await?;

        Ok(ticket)
    }

    /// Update status of a ticket (e.g. open -> in_progress -> resolved -> closed).
    pub async fn update_status(
        db: &DatabaseConnection,
        id: i32,
        status: &str,
        notes: Option<String>,
    ) -> ModelResult<Self> {
        let ticket = Self::find_by_id(db, id).await?;
        let mut active: housing_maintenance_tickets::ActiveModel = ticket.into();
        active.status = ActiveValue::set(status.to_string());
        if status == "resolved" || status == "closed" {
            let now: chrono::DateTime<chrono::FixedOffset> = chrono::Utc::now().fixed_offset();
            active.resolved_at = ActiveValue::set(Some(now));
        }
        if let Some(n) = notes {
            active.notes = ActiveValue::set(Some(n));
        }
        active.update(db).await.map_err(ModelError::from)
    }

    /// Assign a worker node to a ticket.
    pub async fn assign(
        db: &DatabaseConnection,
        id: i32,
        assigned_to_node_id: Option<i32>,
    ) -> ModelResult<Self> {
        let ticket = Self::find_by_id(db, id).await?;
        let mut active: housing_maintenance_tickets::ActiveModel = ticket.into();
        active.assigned_to_node_id = ActiveValue::set(assigned_to_node_id);
        if assigned_to_node_id.is_some() && active.status.as_ref() == "open" {
            active.status = ActiveValue::set("in_progress".to_string());
        }
        active.update(db).await.map_err(ModelError::from)
    }
}
