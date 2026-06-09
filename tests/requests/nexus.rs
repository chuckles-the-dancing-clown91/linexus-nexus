use linexus_nexus::app::App;
use loco_rs::testing::prelude::*;
use serial_test::serial;

use super::prepare_data;

#[tokio::test]
#[serial]
async fn me_standing_commissions_node_and_reads_wallet() {
    request::<App, _, _>(|request, ctx| async move {
        let logged_in = prepare_data::init_user_login(&request, &ctx).await;
        let (auth_key, auth_value) = prepare_data::auth_header(&logged_in.token);

        let response = request
            .get("/api/nexus/me/standing")
            .add_header(auth_key, auth_value)
            .await;

        assert_eq!(
            response.status_code(),
            200,
            "me/standing should succeed for an authenticated user"
        );

        let body: serde_json::Value = serde_json::from_str(&response.text()).unwrap();
        assert_eq!(body["node"]["class"], "human");
        // A fresh node has an empty, fully-guaranteed floor and a zero balance.
        assert_eq!(body["wallet"]["balance"], 0);
        assert_eq!(body["floor"]["healthcare"], true);
        assert!(body["node"]["node_id"].is_string());
    })
    .await;
}

#[tokio::test]
#[serial]
async fn me_standing_requires_auth() {
    request::<App, _, _>(|request, _ctx| async move {
        let response = request.get("/api/nexus/me/standing").await;
        assert_eq!(
            response.status_code(),
            401,
            "me/standing must reject unauthenticated callers"
        );
    })
    .await;
}
