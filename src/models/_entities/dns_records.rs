//! `SeaORM` Entity for dns_records table (records of BIND zones)

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "dns_records")]
pub struct Model {
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(unique)]
    pub record_id: Uuid,
    pub zone_id: Uuid,
    pub record_type: String,
    /// The FQDN, lowercase, without the trailing dot.
    pub name: String,
    #[sea_orm(column_type = "Text")]
    pub content: String,
    pub ttl: i32,
    pub priority: Option<i32>,
    #[sea_orm(column_type = "Text", nullable)]
    pub comment: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
