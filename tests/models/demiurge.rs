use linexus_nexus::{
    app::App,
    models::{contributions, nodes, payments, wallet},
};
use loco_rs::testing::prelude::*;
use serial_test::serial;

/// Commission a human node and return its public UUID.
async fn commission_human(db: &sea_orm::DatabaseConnection, label: &str) -> uuid::Uuid {
    let node = nodes::Model::commission(
        db,
        &nodes::CommissionParams {
            class: "human".to_string(),
            label: label.to_string(),
            lifecycle_phase: Some("labor".to_string()),
            capabilities: None,
            public_key: None,
            owner_user_id: None,
            source: Some("test".to_string()),
            external_ref: Some(label.to_string()),
        },
    )
    .await
    .expect("commission node");
    node.node_id
}

#[tokio::test]
#[serial]
async fn contribution_mints_into_wallet() {
    let boot = boot_test::<App>().await.expect("boot test app");
    let db = &boot.app_context.db;
    let now = chrono::Utc::now().timestamp();

    let node_id = commission_human(db, "alice").await;

    // 20 hours of labor at 5/hr -> 100 Demiurge.
    let (_row, minted) = contributions::Model::record(
        db,
        &contributions::RecordParams {
            node_id,
            kind: "labor".to_string(),
            minutes: 20 * 60,
            essential: false,
            coverage: false,
            week_index: 0,
            note: None,
            at_unix: Some(now),
        },
    )
    .await
    .expect("record contribution");

    assert_eq!(minted, 100);
    assert_eq!(wallet::balance(db, node_id, now).await.unwrap(), 100);
}

#[tokio::test]
#[serial]
async fn fiat_payment_converts_fees_to_demiurge() {
    let boot = boot_test::<App>().await.expect("boot test app");
    let db = &boot.app_context.db;
    let now = chrono::Utc::now().timestamp();

    let seller = commission_human(db, "seller").await;

    // A $40 sale with $3 fee + $2 tax (cents). Default conversion 1.0x ->
    // 500 Demiurge minted to the seller node from the recaptured friction.
    let payment = payments::Model::process(
        db,
        &payments::ProcessParams {
            kind: "sale".to_string(),
            source: Some("test".to_string()),
            payer_ref: Some("buyer".to_string()),
            payer_node_id: None,
            payee_node_id: Some(seller),
            pay_currency: "fiat".to_string(),
            amount_fiat_minor: 4000,
            currency: "USD".to_string(),
            fee_fiat_minor: 300,
            tax_fiat_minor: 200,
            demiurge_amount: 0,
            item_ref: Some("listing-1".to_string()),
            metadata: None,
            fee_conversion_bps: None,
        },
    )
    .await
    .expect("process payment");

    assert_eq!(payment.status, "processed");
    assert_eq!(payment.demiurge_from_fees, 500);
    assert_eq!(wallet::balance(db, seller, now).await.unwrap(), 500);
}

#[tokio::test]
#[serial]
async fn spend_and_age_preserving_transfer() {
    let boot = boot_test::<App>().await.expect("boot test app");
    let db = &boot.app_context.db;
    let now = chrono::Utc::now().timestamp();

    let a = commission_human(db, "transfer-a").await;
    let b = commission_human(db, "transfer-b").await;

    // Mentorship: 20h regular at 10/hr (200) + 40h overtime at 2.0x (800) = 1000.
    contributions::Model::record(
        db,
        &contributions::RecordParams {
            node_id: a,
            kind: "mentorship".to_string(),
            minutes: 60 * 60,
            essential: false,
            coverage: false,
            week_index: 0,
            note: None,
            at_unix: Some(now),
        },
    )
    .await
    .unwrap();
    assert_eq!(wallet::balance(db, a, now).await.unwrap(), 1000);

    // Spend 100 into a sink.
    let spent = wallet::spend(db, a, 100, now).await.unwrap();
    assert!(matches!(spent, wallet::SpendOutcome::Spent { .. }));
    assert_eq!(wallet::balance(db, a, now).await.unwrap(), 900);

    // Transfer 200 to B, preserving age.
    let moved = wallet::transfer(db, a, b, 200, now).await.unwrap();
    assert!(matches!(moved, wallet::SpendOutcome::Spent { .. }));
    assert_eq!(wallet::balance(db, a, now).await.unwrap(), 700);
    assert_eq!(wallet::balance(db, b, now).await.unwrap(), 200);

    // Overspend fails cleanly without touching balances.
    let denied = wallet::spend(db, b, 999, now).await.unwrap();
    assert!(matches!(denied, wallet::SpendOutcome::Insufficient { .. }));
    assert_eq!(wallet::balance(db, b, now).await.unwrap(), 200);
}
