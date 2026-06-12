//! Wallet operations over `demiurge_lots`.
//!
//! A wallet is not a stored number. It is the set of a node's unexpired lots,
//! and a balance is always computed *as of a given moment*. Spending draws from
//! the oldest lot first (FIFO), which rewards circulation; decay removes the
//! oldest lots first, which punishes hoarding. Neither operation ever resets a
//! lot's mint instant, so age is non-transferable.

use loco_rs::prelude::*;
use sea_orm::{ActiveValue, QueryOrder, TransactionTrait};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::_entities::demiurge_lots::{self, Entity};
use crate::demiurge;

/// A point-in-time view of a node's wallet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletOutlook {
    pub node_id: Uuid,
    pub balance: i64,
    pub minted_last_7d: i64,
    pub expiring_within_year: i64,
    pub lot_count: usize,
    pub now_unix: i64,
}

/// Outcome of a spend attempt. The floor is never on the line here — spending
/// Demiurge can only fail to buy a *good*, never to keep a person alive.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum SpendOutcome {
    Spent { spent: i64, balance: i64 },
    Insufficient { shortfall: i64, balance: i64 },
}

/// Mint a lot into a node's wallet. This is the only write path that creates
/// Demiurge, and it is always driven by a breath-backed origin (a contribution,
/// or the fee/tax conversion of a real payment).
pub async fn mint(
    db: &DatabaseConnection,
    node_id: Uuid,
    amount: i64,
    minted_at_unix: i64,
    origin: &str,
    origin_ref: Option<String>,
) -> ModelResult<Option<demiurge_lots::Model>> {
    if amount <= 0 {
        return Ok(None);
    }
    let lot = demiurge_lots::ActiveModel {
        node_id: ActiveValue::set(node_id),
        amount: ActiveValue::set(amount),
        original_amount: ActiveValue::set(amount),
        minted_at_unix: ActiveValue::set(minted_at_unix),
        expires_at_unix: ActiveValue::set(demiurge::expires_at(minted_at_unix)),
        origin: ActiveValue::set(origin.to_string()),
        origin_ref: ActiveValue::set(origin_ref),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(Some(lot))
}

/// All of a node's lots, oldest first.
pub async fn lots(
    db: &DatabaseConnection,
    node_id: Uuid,
) -> ModelResult<Vec<demiurge_lots::Model>> {
    Ok(Entity::find()
        .filter(
            model::query::condition()
                .eq(demiurge_lots::Column::NodeId, node_id)
                .build(),
        )
        .order_by_asc(demiurge_lots::Column::MintedAtUnix)
        .order_by_asc(demiurge_lots::Column::Id)
        .all(db)
        .await?)
}

/// Balance as of `now_unix`: the sum of every lot that has not yet decayed.
pub async fn balance(db: &DatabaseConnection, node_id: Uuid, now_unix: i64) -> ModelResult<i64> {
    let lots = lots(db, node_id).await?;
    Ok(lots
        .iter()
        .filter(|l| l.amount > 0 && now_unix < l.expires_at_unix)
        .map(|l| l.amount)
        .sum())
}

/// A full point-in-time outlook for a node's wallet.
pub async fn outlook(
    db: &DatabaseConnection,
    node_id: Uuid,
    now_unix: i64,
) -> ModelResult<WalletOutlook> {
    let lots = lots(db, node_id).await?;
    let live: Vec<&demiurge_lots::Model> = lots
        .iter()
        .filter(|l| l.amount > 0 && now_unix < l.expires_at_unix)
        .collect();
    let week_ago = now_unix - 7 * demiurge::SECONDS_PER_YEAR / 52;
    let year_horizon = now_unix + demiurge::SECONDS_PER_YEAR;
    Ok(WalletOutlook {
        node_id,
        balance: live.iter().map(|l| l.amount).sum(),
        minted_last_7d: live
            .iter()
            .filter(|l| l.minted_at_unix >= week_ago)
            .map(|l| l.amount)
            .sum(),
        expiring_within_year: live
            .iter()
            .filter(|l| l.expires_at_unix <= year_horizon)
            .map(|l| l.amount)
            .sum(),
        lot_count: live.len(),
        now_unix,
    })
}

/// Remove decayed lots and report the amount lost to decay. Decay is the sink
/// that balances minting; at a stable population the two converge and the
/// supply self-regulates with no central authority printing or tightening.
pub async fn decay_sweep(
    db: &DatabaseConnection,
    node_id: Uuid,
    now_unix: i64,
) -> ModelResult<i64> {
    let lots = lots(db, node_id).await?;
    let mut lost = 0i64;
    for lot in lots {
        if now_unix >= lot.expires_at_unix && lot.amount > 0 {
            lost += lot.amount;
            let mut active: demiurge_lots::ActiveModel = lot.into();
            active.amount = ActiveValue::set(0);
            active.update(db).await?;
        }
    }
    Ok(lost)
}

/// Move `amount` of Demiurge from one wallet to another, oldest lot first,
/// **preserving each lot's original mint instant**. Age is non-transferable: a
/// transferred lot keeps decaying from the breath that created it, so a chain of
/// accounts cannot be used to launder age and dodge decay. Returns the spend
/// outcome on the sending side.
pub async fn transfer(
    db: &DatabaseConnection,
    from: Uuid,
    to: Uuid,
    amount: i64,
    now_unix: i64,
) -> ModelResult<SpendOutcome> {
    let txn = db.begin().await?;

    let lots = Entity::find()
        .filter(
            model::query::condition()
                .eq(demiurge_lots::Column::NodeId, from)
                .build(),
        )
        .order_by_asc(demiurge_lots::Column::MintedAtUnix)
        .order_by_asc(demiurge_lots::Column::Id)
        .all(&txn)
        .await?;

    let available: i64 = lots
        .iter()
        .filter(|l| l.amount > 0 && now_unix < l.expires_at_unix)
        .map(|l| l.amount)
        .sum();

    if amount <= 0 {
        txn.commit().await?;
        return Ok(SpendOutcome::Spent {
            spent: 0,
            balance: available,
        });
    }
    if available < amount {
        txn.rollback().await?;
        return Ok(SpendOutcome::Insufficient {
            shortfall: amount - available,
            balance: available,
        });
    }

    let mut remaining = amount;
    for lot in lots {
        if remaining == 0 {
            break;
        }
        if lot.amount <= 0 || now_unix >= lot.expires_at_unix {
            continue;
        }
        let take = remaining.min(lot.amount);
        remaining -= take;
        let minted_at = lot.minted_at_unix;
        let expires_at = lot.expires_at_unix;
        let origin_ref = lot.origin_ref.clone();

        // Draw down the sender's lot.
        let new_amount = lot.amount - take;
        let mut active: demiurge_lots::ActiveModel = lot.into();
        active.amount = ActiveValue::set(new_amount);
        active.update(&txn).await?;

        // Land an age-identical lot on the receiver.
        demiurge_lots::ActiveModel {
            node_id: ActiveValue::set(to),
            amount: ActiveValue::set(take),
            original_amount: ActiveValue::set(take),
            minted_at_unix: ActiveValue::set(minted_at),
            expires_at_unix: ActiveValue::set(expires_at),
            origin: ActiveValue::set("transfer".to_string()),
            origin_ref: ActiveValue::set(origin_ref),
            ..Default::default()
        }
        .insert(&txn)
        .await?;
    }

    txn.commit().await?;
    Ok(SpendOutcome::Spent {
        spent: amount,
        balance: available - amount,
    })
}

/// Spend `amount` from a node's wallet, oldest lot first. Expired lots are
/// skipped (and zeroed), never spent. Returns whether the spend succeeded.
pub async fn spend(
    db: &DatabaseConnection,
    node_id: Uuid,
    amount: i64,
    now_unix: i64,
) -> ModelResult<SpendOutcome> {
    let txn = db.begin().await?;

    let lots = Entity::find()
        .filter(
            model::query::condition()
                .eq(demiurge_lots::Column::NodeId, node_id)
                .build(),
        )
        .order_by_asc(demiurge_lots::Column::MintedAtUnix)
        .order_by_asc(demiurge_lots::Column::Id)
        .all(&txn)
        .await?;

    let available: i64 = lots
        .iter()
        .filter(|l| l.amount > 0 && now_unix < l.expires_at_unix)
        .map(|l| l.amount)
        .sum();

    if amount <= 0 {
        txn.commit().await?;
        return Ok(SpendOutcome::Spent {
            spent: 0,
            balance: available,
        });
    }

    if available < amount {
        txn.rollback().await?;
        return Ok(SpendOutcome::Insufficient {
            shortfall: amount - available,
            balance: available,
        });
    }

    let mut remaining = amount;
    for lot in lots {
        if remaining == 0 {
            break;
        }
        if lot.amount <= 0 || now_unix >= lot.expires_at_unix {
            continue;
        }
        let take = remaining.min(lot.amount);
        let new_amount = lot.amount - take;
        remaining -= take;
        let mut active: demiurge_lots::ActiveModel = lot.into();
        active.amount = ActiveValue::set(new_amount);
        active.update(&txn).await?;
    }

    txn.commit().await?;
    Ok(SpendOutcome::Spent {
        spent: amount,
        balance: available - amount,
    })
}
