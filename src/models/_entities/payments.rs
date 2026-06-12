//! `SeaORM` Entity for payments table

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "payments")]
pub struct Model {
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
    #[sea_orm(primary_key)]
    pub id: i32,
    pub payment_id: Uuid,
    pub kind: String,
    pub source: Option<String>,
    pub payer_ref: Option<String>,
    pub payee_node_id: Option<Uuid>,
    pub pay_currency: String,
    pub amount_fiat_minor: i64,
    pub currency: String,
    pub fee_fiat_minor: i64,
    pub tax_fiat_minor: i64,
    pub demiurge_amount: i64,
    pub demiurge_from_fees: i64,
    pub status: String,
    pub item_ref: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub metadata: Option<String>,
    pub processed_at: Option<DateTimeWithTimeZone>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
