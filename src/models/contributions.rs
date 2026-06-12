//! Contribution ledger. Recording a contribution is the *only breath-backed*
//! way new Demiurge enters a wallet. Each record is appended, never edited, and
//! drives a mint sized by the canonical formula.

use loco_rs::prelude::*;
use sea_orm::{ActiveValue, QueryOrder, QuerySelect};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use super::_entities::contributions::{self, ActiveModel, Entity, Model};
use crate::demiurge::{self, ContributionKind};
use crate::models::wallet;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RecordParams {
    pub node_id: Uuid,
    /// labor | education | mentorship | care | emergency_response |
    /// rehabilitation | content_creation
    pub kind: String,
    pub minutes: i64,
    #[serde(default)]
    pub essential: bool,
    #[serde(default)]
    pub coverage: bool,
    #[serde(default)]
    pub week_index: i64,
    #[serde(default)]
    pub note: Option<String>,
    /// Unix seconds; defaults to now.
    #[serde(default)]
    pub at_unix: Option<i64>,
}

impl Model {
    /// Overtime-eligible minutes already logged this week for a node, used so
    /// the 20-hour overtime threshold is honored across a week of spans.
    async fn prior_work_minutes(
        db: &DatabaseConnection,
        node_id: Uuid,
        week_index: i64,
    ) -> ModelResult<i64> {
        let rows = Entity::find()
            .filter(
                model::query::condition()
                    .eq(contributions::Column::NodeId, node_id)
                    .eq(contributions::Column::WeekIndex, week_index)
                    .build(),
            )
            .all(db)
            .await?;
        Ok(rows
            .iter()
            .filter(|r| ContributionKind::from_str_lenient(&r.kind).overtime_eligible())
            .map(|r| r.minutes)
            .sum())
    }

    /// Record a contribution and mint the resulting Demiurge into the node's
    /// wallet. Returns the ledger row and the amount minted.
    pub async fn record(
        db: &DatabaseConnection,
        params: &RecordParams,
    ) -> ModelResult<(Self, i64)> {
        let kind = ContributionKind::from_str_lenient(&params.kind);
        let at_unix = params
            .at_unix
            .unwrap_or_else(|| chrono::Utc::now().timestamp());
        let minutes = params.minutes.max(0);

        let prior = Self::prior_work_minutes(db, params.node_id, params.week_index).await?;
        let minted = demiurge::mint_span(kind, minutes, params.essential, params.coverage, prior);

        let contribution_id = Uuid::new_v4();
        let row = contributions::ActiveModel {
            contribution_id: ActiveValue::set(contribution_id),
            node_id: ActiveValue::set(params.node_id),
            kind: ActiveValue::set(params.kind.clone()),
            minutes: ActiveValue::set(minutes),
            essential: ActiveValue::set(params.essential),
            coverage: ActiveValue::set(params.coverage),
            week_index: ActiveValue::set(params.week_index),
            minted_amount: ActiveValue::set(minted),
            at_unix: ActiveValue::set(at_unix),
            note: ActiveValue::set(params.note.clone()),
            ..Default::default()
        }
        .insert(db)
        .await?;

        wallet::mint(
            db,
            params.node_id,
            minted,
            at_unix,
            "contribution",
            Some(contribution_id.to_string()),
        )
        .await?;

        Ok((row, minted))
    }

    /// Recent contributions for a node, newest first.
    pub async fn recent_for_node(
        db: &DatabaseConnection,
        node_id: Uuid,
        limit: u64,
    ) -> ModelResult<Vec<Self>> {
        Ok(Entity::find()
            .filter(
                model::query::condition()
                    .eq(contributions::Column::NodeId, node_id)
                    .build(),
            )
            .order_by_desc(contributions::Column::AtUnix)
            .limit(limit)
            .all(db)
            .await?)
    }
}
