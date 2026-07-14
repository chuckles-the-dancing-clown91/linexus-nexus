//! `SeaORM` Entity for housing_occupancies table

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "housing_occupancies")]
pub struct Model {
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
    #[sea_orm(primary_key)]
    pub id: i32,
    pub occupancy_id: Uuid,
    pub housing_unit_id: i32,
    pub resident_node_id: i32,
    pub assigned_by_node_id: Option<i32>,
    pub assigned_at: DateTimeWithTimeZone,
    /// NULL while currently occupying the unit.
    pub vacated_at: Option<DateTimeWithTimeZone>,
    #[sea_orm(column_type = "Text", nullable)]
    pub notes: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
