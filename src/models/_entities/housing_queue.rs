//! `SeaORM` Entity for housing_queue table

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "housing_queues")]
pub struct Model {
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
    #[sea_orm(primary_key)]
    pub id: i32,
    pub queue_id: Uuid,
    pub housing_unit_id: i32,
    pub unit_label: String,
    /// available | reserved | assigned
    pub status: String,
    pub queued_at: DateTimeWithTimeZone,
    pub claimed_at: Option<DateTimeWithTimeZone>,
    pub claimed_by_node_id: Option<i32>,
    pub priority: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
