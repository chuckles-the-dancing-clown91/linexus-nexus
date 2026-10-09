//! `SeaORM` Entity for provider_credentials table

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "provider_credentials")]
pub struct Model {
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(unique)]
    pub provider: String,
    /// AES-256-GCM sealed token (nonce ‖ ciphertext). Never serialized.
    #[sea_orm(column_type = "Blob", nullable)]
    #[serde(skip)]
    pub sealed_token: Option<Vec<u8>>,
    pub account_id: Option<String>,
    pub account_name: Option<String>,
    pub state: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub detail: Option<String>,
    pub checked_at: Option<DateTimeWithTimeZone>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
