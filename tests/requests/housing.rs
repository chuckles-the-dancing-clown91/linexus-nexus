//! Housing commons request tests: node lifecycle, council quorum, resident
//! assignment / vacate + re-queue, and RBAC permission guards.

use axum::http::{HeaderName, HeaderValue};
use linexus_nexus::app::App;
use linexus_nexus::middleware::rbac;
use linexus_nexus::models::{nodes, roles, users, wallet};
use loco_rs::{app::AppContext, testing::prelude::*, TestServer};
use serial_test::serial;

struct Member {
    user: users::Model,
    token: String,
}

/// Register, verify and log in a user with the given identity.
async fn register_login(request: &TestServer, ctx: &AppContext, name: &str, email: &str) -> Member {
    request
        .post("/api/auth/register")
        .json(&serde_json::json!({
            "name": name,
            "email": email,
            "password": "housing1234"
        }))
        .await;
    let user = users::Model::find_by_email(&ctx.db, email).await.unwrap();
    request
        .post("/api/auth/verify")
        .json(&serde_json::json!({ "token": user.email_verification_token }))
        .await;
    let response = request
        .post("/api/auth/login")
        .json(&serde_json::json!({ "email": email, "password": "housing1234" }))
        .await;
    let body: serde_json::Value = serde_json::from_str(&response.text()).unwrap();
    let token = body["token"].as_str().unwrap().to_string();
    let user = users::Model::find_by_email(&ctx.db, email).await.unwrap();
    Member { user, token }
}

/// Register a user, grant them a seeded role, and commission an owned node.
async fn member_with_role(
    request: &TestServer,
    ctx: &AppContext,
    name: &str,
    email: &str,
    role: &str,
    node_class: &str,
) -> Member {
    // Roles are seeded at boot, but seed again defensively (idempotent).
    roles::Model::seed_defaults(&ctx.db).await.unwrap();
    let member = register_login(request, ctx, name, email).await;
    rbac::assign_role(&ctx.db, member.user.id, role)
        .await
        .unwrap();
    nodes::Model::commission(
        &ctx.db,
        &nodes::CommissionParams {
            class: node_class.to_string(),
            label: name.to_string(),
            lifecycle_phase: Some("active".to_string()),
            capabilities: None,
            public_key: None,
            owner_user_id: Some(member.user.id),
            source: Some("test".to_string()),
            external_ref: Some(email.to_string()),
        },
    )
    .await
    .unwrap();
    member
}

fn bearer(token: &str) -> (HeaderName, HeaderValue) {
    (
        HeaderName::from_static("authorization"),
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    )
}

async fn get_json(request: &TestServer, path: &str, token: &str) -> serde_json::Value {
    let (k, v) = bearer(token);
    let response = request.get(path).add_header(k, v).await;
    assert_eq!(
        response.status_code(),
        200,
        "GET {path} failed: {}",
        response.text()
    );
    serde_json::from_str(&response.text()).unwrap()
}

/// Drive a housing node through create → units → submit → 2 council approvals.
/// Returns (housing_node_id, unit_id, council votes' tokens).
async fn activate_block(
    request: &TestServer,
    ctx: &AppContext,
    manager: &Member,
    council: &[&Member],
) -> (i64, i64) {
    let (k, v) = bearer(&manager.token);
    let response = request
        .post("/api/housing/nodes")
        .add_header(k, v)
        .json(&serde_json::json!({
            "name": "Homestead Block · H-1x",
            "address": "1 Corridor North",
            "city": "Corridor-North",
            "property_type": "multi_family",
            "unit_count": 2
        }))
        .await;
    assert_eq!(
        response.status_code(),
        200,
        "create node: {}",
        response.text()
    );
    let node: serde_json::Value = serde_json::from_str(&response.text()).unwrap();
    assert_eq!(node["status"], "draft");
    assert_eq!(node["quorum_required"], 2);
    let node_id = node["id"].as_i64().unwrap();

    let (k, v) = bearer(&manager.token);
    let response = request
        .post(&format!("/api/housing/nodes/{node_id}/units"))
        .add_header(k, v)
        .json(&serde_json::json!({ "unit_number": "H-12", "beds": 2, "baths": 1.0 }))
        .await;
    assert_eq!(response.status_code(), 200, "add unit: {}", response.text());
    let unit: serde_json::Value = serde_json::from_str(&response.text()).unwrap();
    assert_eq!(unit["status"], "available");
    let unit_id = unit["id"].as_i64().unwrap();

    // Draft-node units must NOT be in the commons queue yet.
    let queue = get_json(request, "/api/housing/queue", &manager.token).await;
    assert_eq!(
        queue.as_array().unwrap().len(),
        0,
        "queue must stay empty pre-activation"
    );

    let (k, v) = bearer(&manager.token);
    let response = request
        .patch(&format!("/api/housing/nodes/{node_id}/submit"))
        .add_header(k, v)
        .await;
    assert_eq!(response.status_code(), 200, "submit: {}", response.text());
    let node: serde_json::Value = serde_json::from_str(&response.text()).unwrap();
    assert_eq!(node["status"], "pending_council");
    assert!(!node["submitted_at"].is_null());

    for member in council {
        let (k, v) = bearer(&member.token);
        let response = request
            .post(&format!("/api/housing/nodes/{node_id}/review"))
            .add_header(k, v)
            .json(&serde_json::json!({ "vote": "approve" }))
            .await;
        assert_eq!(response.status_code(), 200, "vote: {}", response.text());
    }
    let _ = ctx;

    (node_id, unit_id)
}

#[tokio::test]
#[serial]
async fn housing_endpoints_require_permission() {
    request::<App, _, _>(|request, ctx| async move {
        // Unauthenticated → 401.
        let response = request.get("/api/housing/nodes").await;
        assert_eq!(response.status_code(), 401);

        // Authenticated but free-plan, role-less user → 401 (no housing:read).
        let plain = register_login(&request, &ctx, "icarus", "icarus@corridor.north").await;
        let (k, v) = bearer(&plain.token);
        let response = request.get("/api/housing/nodes").add_header(k, v).await;
        assert_eq!(
            response.status_code(),
            401,
            "free user must lack housing:read"
        );

        let (k, v) = bearer(&plain.token);
        let response = request
            .post("/api/housing/nodes")
            .add_header(k, v)
            .json(&serde_json::json!({
                "name": "x", "address": "x", "property_type": "condo", "unit_count": 1
            }))
            .await;
        assert_eq!(
            response.status_code(),
            401,
            "free user must lack housing:write"
        );
    })
    .await;
}

#[tokio::test]
#[serial]
async fn lifecycle_quorum_activates_and_mints_civic_labor() {
    request::<App, _, _>(|request, ctx| async move {
        let manager = member_with_role(
            &request,
            &ctx,
            "daedalus",
            "daedalus@corridor.north",
            "housing_manager",
            "human",
        )
        .await;
        let ariadne = member_with_role(
            &request,
            &ctx,
            "ariadne",
            "ariadne@corridor.north",
            "council",
            "council",
        )
        .await;
        let minos = member_with_role(
            &request,
            &ctx,
            "minos",
            "minos@corridor.north",
            "council",
            "council",
        )
        .await;

        // First approval alone must not activate (quorum = 2).
        let (node_id, _unit_id) = {
            let (k, v) = bearer(&manager.token);
            let response = request
                .post("/api/housing/nodes")
                .add_header(k, v)
                .json(&serde_json::json!({
                    "name": "Homestead Block · H-1x",
                    "address": "1 Corridor North",
                    "property_type": "multi_family",
                    "unit_count": 2
                }))
                .await;
            let node: serde_json::Value = serde_json::from_str(&response.text()).unwrap();
            let node_id = node["id"].as_i64().unwrap();

            let (k, v) = bearer(&manager.token);
            let response = request
                .post(&format!("/api/housing/nodes/{node_id}/units"))
                .add_header(k, v)
                .json(&serde_json::json!({ "unit_number": "H-12" }))
                .await;
            let unit: serde_json::Value = serde_json::from_str(&response.text()).unwrap();

            let (k, v) = bearer(&manager.token);
            request
                .patch(&format!("/api/housing/nodes/{node_id}/submit"))
                .add_header(k, v)
                .await;

            let (k, v) = bearer(&ariadne.token);
            let response = request
                .post(&format!("/api/housing/nodes/{node_id}/review"))
                .add_header(k, v)
                .json(&serde_json::json!({ "vote": "approve" }))
                .await;
            assert_eq!(response.status_code(), 200);

            let node = get_json(
                &request,
                &format!("/api/housing/nodes/{node_id}"),
                &manager.token,
            )
            .await;
            assert_eq!(
                node["status"], "pending_council",
                "one approval must not meet quorum"
            );

            (node_id, unit["id"].as_i64().unwrap())
        };

        // Second approval meets quorum → active, units enter the queue.
        let (k, v) = bearer(&minos.token);
        let response = request
            .post(&format!("/api/housing/nodes/{node_id}/review"))
            .add_header(k, v)
            .json(&serde_json::json!({ "vote": "approve" }))
            .await;
        assert_eq!(response.status_code(), 200);

        let node = get_json(
            &request,
            &format!("/api/housing/nodes/{node_id}"),
            &manager.token,
        )
        .await;
        assert_eq!(node["status"], "active");
        assert!(!node["activated_at"].is_null());

        let queue = get_json(&request, "/api/housing/queue", &manager.token).await;
        let queue = queue.as_array().unwrap();
        assert_eq!(queue.len(), 1, "activation must enqueue the available unit");
        assert_eq!(queue[0]["unit_label"], "H-12");
        assert_eq!(queue[0]["status"], "available");

        // The decisive vote is civic labor: Minos' node wallet was minted to.
        let minos_node = nodes::Model::find_by_owner(&ctx.db, minos.user.id)
            .await
            .unwrap()
            .unwrap();
        let balance = wallet::balance(&ctx.db, minos_node.node_id, chrono::Utc::now().timestamp())
            .await
            .unwrap();
        assert!(
            balance > 0,
            "decisive council vote must mint civic-labor Demiurge"
        );

        // Votes are idempotent — a re-vote records nothing new.
        let (k, v) = bearer(&minos.token);
        let response = request
            .post(&format!("/api/housing/nodes/{node_id}/review"))
            .add_header(k, v)
            .json(&serde_json::json!({ "vote": "reject" }))
            .await;
        assert_eq!(response.status_code(), 200);
        let reviews = get_json(
            &request,
            &format!("/api/housing/nodes/{node_id}/reviews"),
            &manager.token,
        )
        .await;
        assert_eq!(
            reviews.as_array().unwrap().len(),
            2,
            "re-vote must not add a review"
        );
        assert!(reviews
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["vote"] == "approve"));
    })
    .await;
}

#[tokio::test]
#[serial]
async fn assign_and_vacate_requeues_unit() {
    request::<App, _, _>(|request, ctx| async move {
        let manager = member_with_role(
            &request,
            &ctx,
            "daedalus",
            "daedalus@corridor.north",
            "housing_manager",
            "human",
        )
        .await;
        let ariadne = member_with_role(
            &request,
            &ctx,
            "ariadne",
            "ariadne@corridor.north",
            "council",
            "council",
        )
        .await;
        let minos = member_with_role(
            &request,
            &ctx,
            "minos",
            "minos@corridor.north",
            "council",
            "council",
        )
        .await;
        let (_node_id, unit_id) =
            activate_block(&request, &ctx, &manager, &[&ariadne, &minos]).await;

        // A resident node for Theseus (no login needed — assignment is by node).
        let theseus = nodes::Model::commission(
            &ctx.db,
            &nodes::CommissionParams {
                class: "human".to_string(),
                label: "theseus".to_string(),
                lifecycle_phase: Some("active".to_string()),
                capabilities: None,
                public_key: None,
                owner_user_id: None,
                source: Some("test".to_string()),
                external_ref: Some("theseus".to_string()),
            },
        )
        .await
        .unwrap();

        // The housing manager may not assign — that is council stewardship.
        let (k, v) = bearer(&manager.token);
        let response = request
            .post(&format!("/api/housing/units/{unit_id}/assign"))
            .add_header(k, v)
            .json(&serde_json::json!({ "resident_node_id": theseus.id }))
            .await;
        assert_eq!(
            response.status_code(),
            401,
            "manager must lack housing:assign"
        );

        // Council assigns Theseus to H-12.
        let (k, v) = bearer(&ariadne.token);
        let response = request
            .post(&format!("/api/housing/units/{unit_id}/assign"))
            .add_header(k, v)
            .json(&serde_json::json!({ "resident_node_id": theseus.id }))
            .await;
        assert_eq!(response.status_code(), 200, "assign: {}", response.text());
        let occupancy: serde_json::Value = serde_json::from_str(&response.text()).unwrap();
        assert_eq!(occupancy["resident_node_id"], theseus.id);
        assert!(occupancy["vacated_at"].is_null());

        let queue = get_json(&request, "/api/housing/queue", &manager.token).await;
        assert_eq!(
            queue.as_array().unwrap().len(),
            0,
            "assigned unit must leave the queue"
        );

        // Double-assignment of an occupied unit must fail.
        let (k, v) = bearer(&ariadne.token);
        let response = request
            .post(&format!("/api/housing/units/{unit_id}/assign"))
            .add_header(k, v)
            .json(&serde_json::json!({ "resident_node_id": theseus.id }))
            .await;
        assert_ne!(
            response.status_code(),
            200,
            "occupied unit must refuse assignment"
        );

        // Vacate → occupancy closed, unit make_ready, unit re-queued.
        let (k, v) = bearer(&ariadne.token);
        let response = request
            .post(&format!("/api/housing/units/{unit_id}/vacate"))
            .add_header(k, v)
            .json(&serde_json::json!({ "notes": "lean opened on H-12" }))
            .await;
        assert_eq!(response.status_code(), 200, "vacate: {}", response.text());
        let occupancy: serde_json::Value = serde_json::from_str(&response.text()).unwrap();
        assert!(!occupancy["vacated_at"].is_null());

        let queue = get_json(&request, "/api/housing/queue", &manager.token).await;
        let queue = queue.as_array().unwrap();
        assert_eq!(queue.len(), 1, "vacated unit must re-enter the queue");
        assert_eq!(queue[0]["unit_label"], "H-12");
    })
    .await;
}

#[tokio::test]
#[serial]
async fn non_council_cannot_review() {
    request::<App, _, _>(|request, ctx| async move {
        let manager = member_with_role(
            &request,
            &ctx,
            "daedalus",
            "daedalus@corridor.north",
            "housing_manager",
            "human",
        )
        .await;

        let (k, v) = bearer(&manager.token);
        let response = request
            .post("/api/housing/nodes")
            .add_header(k, v)
            .json(&serde_json::json!({
                "name": "Homestead Block · H-1x",
                "address": "1 Corridor North",
                "property_type": "multi_family",
                "unit_count": 1
            }))
            .await;
        let node: serde_json::Value = serde_json::from_str(&response.text()).unwrap();
        let node_id = node["id"].as_i64().unwrap();

        let (k, v) = bearer(&manager.token);
        request
            .patch(&format!("/api/housing/nodes/{node_id}/submit"))
            .add_header(k, v)
            .await;

        // housing_manager has no housing:review.
        let (k, v) = bearer(&manager.token);
        let response = request
            .post(&format!("/api/housing/nodes/{node_id}/review"))
            .add_header(k, v)
            .json(&serde_json::json!({ "vote": "approve" }))
            .await;
        assert_eq!(
            response.status_code(),
            401,
            "manager must not hold housing:review"
        );
    })
    .await;
}
