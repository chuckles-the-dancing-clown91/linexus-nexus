//! Housing documents model — links documents/resolutions to housing nodes/units.

use loco_rs::prelude::*;
use sea_orm::QueryOrder;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use super::_entities::housing_documents::{self, ActiveModel, Entity, Model};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreateDocumentParams {
    pub housing_node_id: i32,
    pub housing_unit_id: Option<i32>,
    /// council_resolution | inspection_report | occupancy_agreement | maintenance_report | other
    pub document_kind: String,
    pub title: String,
    pub url: Option<String>,
    pub content: Option<String>,
    pub uploaded_by_node_id: Option<i32>,
}

impl Model {
    pub async fn find_by_id(db: &DatabaseConnection, id: i32) -> ModelResult<Self> {
        housing_documents::Entity::find_by_id(id)
            .one(db)
            .await?
            .ok_or_else(|| ModelError::EntityNotFound)
    }

    /// List all documents for a housing node.
    pub async fn find_for_node(
        db: &DatabaseConnection,
        housing_node_id: i32,
    ) -> ModelResult<Vec<Self>> {
        Ok(housing_documents::Entity::find()
            .filter(
                model::query::condition()
                    .eq(housing_documents::Column::HousingNodeId, housing_node_id)
                    .build(),
            )
            .order_by_desc(housing_documents::Column::UploadedAt)
            .all(db)
            .await?)
    }

    /// Create a document.
    pub async fn create(db: &DatabaseConnection, params: &CreateDocumentParams) -> ModelResult<Self> {
        let now: chrono::DateTime<chrono::FixedOffset> =
            chrono::Utc::now().fixed_offset().into();

        let doc = housing_documents::ActiveModel {
            document_id: sea_orm::ActiveValue::set(Uuid::new_v4()),
            housing_node_id: sea_orm::ActiveValue::set(params.housing_node_id),
            housing_unit_id: sea_orm::ActiveValue::set(params.housing_unit_id),
            document_kind: sea_orm::ActiveValue::set(params.document_kind.clone()),
            title: sea_orm::ActiveValue::set(params.title.clone()),
            url: sea_orm::ActiveValue::set(params.url.clone()),
            content: sea_orm::ActiveValue::set(params.content.clone()),
            uploaded_by_node_id: sea_orm::ActiveValue::set(params.uploaded_by_node_id),
            uploaded_at: sea_orm::ActiveValue::set(now),
            ..Default::default()
        }
        .insert(db)
        .await?;

        Ok(doc)
    }
}
