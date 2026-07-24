//! Housing units model — individual occupiable spaces within a housing node.

use loco_rs::prelude::*;
use sea_orm::{ActiveValue, ConnectionTrait, QueryOrder};
use serde::{Deserialize, Serialize};

pub use super::_entities::housing_units::{self, ActiveModel, Entity, Model};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitStatus {
    Available,
    Occupied,
    MakeReady,
    Maintenance,
    Down,
}

impl UnitStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Occupied => "occupied",
            Self::MakeReady => "make_ready",
            Self::Maintenance => "maintenance",
            Self::Down => "down",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreateUnitParams {
    pub housing_node_id: i32,
    pub unit_number: String,
    #[serde(default)]
    pub beds: Option<i32>,
    #[serde(default)]
    pub baths: Option<f64>,
    #[serde(default)]
    pub sqft: Option<i32>,
    #[serde(default)]
    pub notes: Option<String>,
}

impl Model {
    pub async fn find_by_id<C: ConnectionTrait>(db: &C, id: i32) -> ModelResult<Self> {
        housing_units::Entity::find_by_id(id)
            .one(db)
            .await?
            .ok_or_else(|| ModelError::EntityNotFound)
    }

    /// All units belonging to a housing node.
    pub async fn find_for_node<C: ConnectionTrait>(
        db: &C,
        housing_node_id: i32,
    ) -> ModelResult<Vec<Self>> {
        Ok(housing_units::Entity::find()
            .filter(
                model::query::condition()
                    .eq(housing_units::Column::HousingNodeId, housing_node_id)
                    .build(),
            )
            .order_by_asc(housing_units::Column::UnitNumber)
            .all(db)
            .await?)
    }

    /// All units with `available` status (for queue display).
    pub async fn find_available<C: ConnectionTrait>(db: &C) -> ModelResult<Vec<Self>> {
        Ok(housing_units::Entity::find()
            .filter(
                model::query::condition()
                    .eq(housing_units::Column::Status, "available")
                    .build(),
            )
            .all(db)
            .await?)
    }

    /// Create a unit and default it to `available`.
    pub async fn create<C: ConnectionTrait>(
        db: &C,
        params: &CreateUnitParams,
    ) -> ModelResult<Self> {
        let unit = housing_units::ActiveModel {
            housing_node_id: ActiveValue::set(params.housing_node_id),
            unit_number: ActiveValue::set(params.unit_number.clone()),
            beds: ActiveValue::set(params.beds),
            baths: ActiveValue::set(params.baths),
            sqft: ActiveValue::set(params.sqft),
            status: ActiveValue::set(UnitStatus::Available.as_str().to_string()),
            notes: ActiveValue::set(params.notes.clone()),
            ..Default::default()
        }
        .insert(db)
        .await?;
        Ok(unit)
    }

    /// Transition a unit's status. Direct column write — no enum guard here;
    /// the caller (controller / occupancy model) is responsible for validity.
    pub async fn set_status<C: ConnectionTrait>(
        db: &C,
        id: i32,
        status: &str,
    ) -> ModelResult<Self> {
        let unit = Self::find_by_id(db, id).await?;
        let mut active: housing_units::ActiveModel = unit.into();
        active.status = ActiveValue::set(status.to_string());
        active.update(db).await.map_err(ModelError::from)
    }
}
