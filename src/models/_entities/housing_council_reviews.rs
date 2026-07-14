//! `SeaORM` Entity for housing_council_reviews table

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "housing_council_reviews")]
pub struct Model {
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
    #[sea_orm(primary_key)]
    pub id: i32,
    pub review_id: Uuid,
    pub housing_node_id: i32,
    /// `nodes`.id of the council member.
    pub reviewer_node_id: i32,
    /// approve | reject
    pub vote: String,
    pub voted_at: DateTimeWithTimeZone,
    #[sea_orm(column_type = "Text", nullable)]
    pub notes: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
