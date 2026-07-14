//! `SeaORM` Entity for housing_documents table

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "housing_documents")]
pub struct Model {
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
    #[sea_orm(primary_key)]
    pub id: i32,
    pub document_id: Uuid,
    pub housing_node_id: i32,
    pub housing_unit_id: Option<i32>,
    /// council_resolution | inspection_report | occupancy_agreement |
    /// maintenance_report | other
    pub document_kind: String,
    pub title: String,
    pub url: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub content: Option<String>,
    pub uploaded_by_node_id: Option<i32>,
    pub uploaded_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
