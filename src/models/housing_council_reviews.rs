//! Housing council reviews — one vote per council member per housing node.
//!
//! Casting a vote is idempotent on (housing_node_id, reviewer_node_id): if the
//! member has already voted their existing vote is returned unchanged, enforced
//! by the database's unique index. Changing a vote is intentionally not
//! supported — the immutable ledger guarantees audit integrity.

use loco_rs::prelude::*;
use sea_orm::{ActiveValue, PaginatorTrait, ConnectionTrait};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use super::_entities::housing_council_reviews::{self, ActiveModel, Entity, Model};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CastVoteParams {
    pub housing_node_id: i32,
    /// `nodes`.id of the council member (must be class=council).
    pub reviewer_node_id: i32,
    /// "approve" | "reject"
    pub vote: String,
    #[serde(default)]
    pub notes: Option<String>,
}

impl Model {
    /// Find all reviews for a given housing node.
    pub async fn find_for_node<C: ConnectionTrait>(
        db: &C,
        housing_node_id: i32,
    ) -> ModelResult<Vec<Self>> {
        Ok(housing_council_reviews::Entity::find()
            .filter(
                model::query::condition()
                    .eq(housing_council_reviews::Column::HousingNodeId, housing_node_id)
                    .build(),
            )
            .all(db)
            .await?)
    }

    /// Count the approvals already cast for a housing node.
    pub async fn count_approvals<C: ConnectionTrait>(
        db: &C,
        housing_node_id: i32,
    ) -> ModelResult<i64> {
        let count = housing_council_reviews::Entity::find()
            .filter(
                model::query::condition()
                    .eq(housing_council_reviews::Column::HousingNodeId, housing_node_id)
                    .eq(housing_council_reviews::Column::Vote, "approve")
                    .build(),
            )
            .count(db)
            .await?;
        Ok(count as i64)
    }

    /// Check whether a council member has already voted on a node.
    pub async fn find_existing_vote<C: ConnectionTrait>(
        db: &C,
        housing_node_id: i32,
        reviewer_node_id: i32,
    ) -> ModelResult<Option<Self>> {
        Ok(housing_council_reviews::Entity::find()
            .filter(
                model::query::condition()
                    .eq(housing_council_reviews::Column::HousingNodeId, housing_node_id)
                    .eq(housing_council_reviews::Column::ReviewerNodeId, reviewer_node_id)
                    .build(),
            )
            .one(db)
            .await?)
    }

    /// Cast a vote. Idempotent: if the member already voted, return the
    /// existing record without error.  To update a vote, a new review cycle
    /// must be opened — immutability is by design.
    pub async fn cast_vote<C: ConnectionTrait>(
        db: &C,
        params: &CastVoteParams,
    ) -> ModelResult<(Self, bool)> {
        // Idempotency guard.
        if let Some(existing) =
            Self::find_existing_vote(db, params.housing_node_id, params.reviewer_node_id).await?
        {
            return Ok((existing, false)); // false = no new vote was cast
        }

        if params.vote != "approve" && params.vote != "reject" {
            return Err(ModelError::msg("vote must be 'approve' or 'reject'"));
        }

        let now: chrono::DateTime<chrono::FixedOffset> =
            chrono::Utc::now().fixed_offset().into();

        let review = housing_council_reviews::ActiveModel {
            review_id: ActiveValue::set(Uuid::new_v4()),
            housing_node_id: ActiveValue::set(params.housing_node_id),
            reviewer_node_id: ActiveValue::set(params.reviewer_node_id),
            vote: ActiveValue::set(params.vote.clone()),
            voted_at: ActiveValue::set(now),
            notes: ActiveValue::set(params.notes.clone()),
            ..Default::default()
        }
        .insert(db)
        .await?;

        Ok((review, true)) // true = fresh vote recorded
    }
}
