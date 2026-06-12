//! Payments routed through the Nexus on behalf of the Vicinagora marketplace.
//!
//! A payment settles in fiat or in Demiurge:
//!
//! * **fiat** — the principal is captured by the external processor (simulated
//!   here behind a clean seam). The *fees and taxes* are converted to Demiurge
//!   and minted to the payee node, so the friction the old world skims becomes
//!   contribution credit inside the node instead of leaking out of it.
//! * **demiurge** — value moves between wallets directly, age-preserving, with
//!   no fiat friction to convert.

use loco_rs::prelude::*;
use sea_orm::{ActiveValue, QueryOrder, QuerySelect};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use super::_entities::payments::{self, ActiveModel, Entity, Model};
use crate::demiurge;
use crate::models::wallet::{self, SpendOutcome};

/// Default conversion: 1 fiat minor-unit (cent) of friction -> 1 Demiurge.
/// Governance-tunable; the caller may override per payment.
pub const DEFAULT_FEE_CONVERSION_BPS: i64 = demiurge::ONE_BPS;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProcessParams {
    /// sale | donation | redemption | subscription
    pub kind: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub payer_ref: Option<String>,
    /// Wallet that pays, for demiurge settlement.
    #[serde(default)]
    pub payer_node_id: Option<Uuid>,
    /// Wallet that receives recaptured fees (fiat) or transferred value.
    #[serde(default)]
    pub payee_node_id: Option<Uuid>,
    /// fiat | demiurge
    pub pay_currency: String,
    #[serde(default)]
    pub amount_fiat_minor: i64,
    #[serde(default = "default_currency")]
    pub currency: String,
    #[serde(default)]
    pub fee_fiat_minor: i64,
    #[serde(default)]
    pub tax_fiat_minor: i64,
    #[serde(default)]
    pub demiurge_amount: i64,
    #[serde(default)]
    pub item_ref: Option<String>,
    #[serde(default)]
    pub metadata: Option<String>,
    /// Override the fee->Demiurge conversion rate (basis points).
    #[serde(default)]
    pub fee_conversion_bps: Option<i64>,
}

fn default_currency() -> String {
    "USD".to_string()
}

impl Model {
    pub async fn find_by_payment_id(
        db: &DatabaseConnection,
        payment_id: &Uuid,
    ) -> ModelResult<Self> {
        Entity::find()
            .filter(
                model::query::condition()
                    .eq(payments::Column::PaymentId, *payment_id)
                    .build(),
            )
            .one(db)
            .await?
            .ok_or_else(|| ModelError::EntityNotFound)
    }

    pub async fn find_all(db: &DatabaseConnection, limit: u64) -> ModelResult<Vec<Self>> {
        Ok(Entity::find()
            .order_by_desc(payments::Column::Id)
            .limit(limit)
            .all(db)
            .await?)
    }

    /// Process a payment, performing the wallet movements and fee conversion,
    /// then persisting an auditable record. The returned model carries the final
    /// `status` ("processed" or "failed") and the `demiurge_from_fees` minted.
    pub async fn process(db: &DatabaseConnection, params: &ProcessParams) -> ModelResult<Self> {
        let now = chrono::Utc::now().timestamp();
        let payment_id = Uuid::new_v4();
        let conversion = params
            .fee_conversion_bps
            .unwrap_or(DEFAULT_FEE_CONVERSION_BPS);

        let mut demiurge_from_fees = 0i64;
        let mut status = "processed";

        match params.pay_currency.as_str() {
            "demiurge" => {
                // Direct wallet settlement.
                if let Some(payer) = params.payer_node_id {
                    let outcome = match params.payee_node_id {
                        // Resident-to-resident: preserve age on transfer.
                        Some(payee) => {
                            wallet::transfer(db, payer, payee, params.demiurge_amount, now).await?
                        }
                        // Into a sink (a good is not a breath): absorb.
                        None => wallet::spend(db, payer, params.demiurge_amount, now).await?,
                    };
                    if matches!(outcome, SpendOutcome::Insufficient { .. }) {
                        status = "failed";
                    }
                } else {
                    status = "failed";
                }
            }
            _ => {
                // Fiat settlement. The principal clears externally; we recapture
                // the friction. Fees + taxes -> Demiurge, minted to the payee.
                let friction = params.fee_fiat_minor.max(0) + params.tax_fiat_minor.max(0);
                demiurge_from_fees = demiurge::fiat_minor_to_demiurge(friction, conversion);
                if let Some(payee) = params.payee_node_id {
                    wallet::mint(
                        db,
                        payee,
                        demiurge_from_fees,
                        now,
                        "payment_fee",
                        Some(payment_id.to_string()),
                    )
                    .await?;
                }
            }
        }

        let processed_at = if status == "processed" {
            Some(chrono::Local::now().into())
        } else {
            None
        };

        let row = payments::ActiveModel {
            payment_id: ActiveValue::set(payment_id),
            kind: ActiveValue::set(params.kind.clone()),
            source: ActiveValue::set(params.source.clone()),
            payer_ref: ActiveValue::set(params.payer_ref.clone()),
            payee_node_id: ActiveValue::set(params.payee_node_id),
            pay_currency: ActiveValue::set(params.pay_currency.clone()),
            amount_fiat_minor: ActiveValue::set(params.amount_fiat_minor),
            currency: ActiveValue::set(params.currency.clone()),
            fee_fiat_minor: ActiveValue::set(params.fee_fiat_minor),
            tax_fiat_minor: ActiveValue::set(params.tax_fiat_minor),
            demiurge_amount: ActiveValue::set(params.demiurge_amount),
            demiurge_from_fees: ActiveValue::set(demiurge_from_fees),
            status: ActiveValue::set(status.to_string()),
            item_ref: ActiveValue::set(params.item_ref.clone()),
            metadata: ActiveValue::set(params.metadata.clone()),
            processed_at: ActiveValue::set(processed_at),
            ..Default::default()
        }
        .insert(db)
        .await?;

        Ok(row)
    }
}
