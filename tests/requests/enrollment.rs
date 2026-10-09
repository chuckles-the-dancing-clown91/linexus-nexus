//! Enrollment tokens, per-agent credentials, richer facts, and the task
//! retry / cancel / hostgroup-target behaviour (`docs/PROVIDERS.md` §1–3).

use linexus_nexus::{app::App, dispatch, models::tasks};
use loco_rs::{testing::prelude::*, TestServer};
use sea_orm::{ActiveModelTrait, ActiveValue};
use serde_json::{json, Value};
use serial_test::serial;

use super::stub::{bearer, bearer_of, Env, Stub};

async fn post(request: &TestServer, path: &str, auth: Option<&str>, body: &Value) -> (u16, Value) {
    let mut req = request.post(path).json(body);
    if let Some(t) = auth {
        let (k, v) = bearer_of(t);
        req = req.add_header(k, v);
    }
    let resp = req.await;
    let status = resp.status_code().as_u16();
    (
        status,
        serde_json::from_str(&resp.text()).unwrap_or(Value::Null),
    )
}

async fn get(request: &TestServer, path: &str, auth: &str) -> (u16, Value) {
    let (k, v) = bearer_of(auth);
    let resp = request.get(path).add_header(k, v).await;
    let status = resp.status_code().as_u16();
    (
        status,
        serde_json::from_str(&resp.text()).unwrap_or(Value::Null),
    )
}

fn root() -> String {
    bearer()
        .1
        .to_str()
        .unwrap()
        .trim_start_matches("Bearer ")
        .to_string()
}

async fn mint(request: &TestServer, body: Value) -> Value {
    let (status, token) = post(request, "/api/v1/enrollment-tokens", Some(&root()), &body).await;
    assert_eq!(status, 201, "{token}");
    token
}

#[tokio::test]
#[serial]
async fn enrollment_token_flow() {
    let _env = Env::new();
    request::<App, _, _>(|request, _ctx| async move {
        let root = root();
        let token = mint(
            &request,
            json!({"hostgroup": "acme", "environment": "staging", "label": "web-1",
                   "metadata": {"clientId": "c-1"}}),
        )
        .await;
        let plaintext = token["token"].as_str().unwrap().to_string();
        assert!(plaintext.starts_with("nxe_"));
        assert_eq!(token["uses"], 0);
        assert_eq!(token["maxUses"], 1);
        assert_eq!(token["agents"], json!([]));
        assert_eq!(token["revokedAt"], Value::Null);
        let token_id = token["id"].as_str().unwrap().to_string();

        // Bad mint bodies.
        let (s, b) = post(
            &request,
            "/api/v1/enrollment-tokens",
            Some(&root),
            &json!({}),
        )
        .await;
        assert_eq!((s, b["error"].as_str()), (400, Some("invalid")));
        let (s, _) = post(
            &request,
            "/api/v1/enrollment-tokens",
            Some(&root),
            &json!({"hostgroup": "acme", "ttlMinutes": 1}),
        )
        .await;
        assert_eq!(s, 400);

        // Enroll with the token and no bearer: the token's placement wins.
        let (s, enrolled) = post(
            &request,
            "/api/v1/agents/enroll",
            None,
            &json!({
                "hostname": "web-1", "hostgroup": "ignored", "machineId": "4c4c4544-0001",
                "enrollmentToken": plaintext,
            }),
        )
        .await;
        assert_eq!(s, 201, "{enrolled}");
        assert_eq!(enrolled["hostgroup"], "acme");
        assert_eq!(enrolled["environment"], "staging");
        assert_eq!(enrolled["readopted"], false);
        assert_eq!(enrolled["enrollmentTokenId"], token_id.as_str());
        assert_eq!(enrolled["metadata"], json!({"clientId": "c-1"}));
        assert_eq!(enrolled["agentId"], enrolled["id"]);
        let agent_id = enrolled["agentId"].as_str().unwrap().to_string();
        let agent_token = enrolled["agentToken"].as_str().unwrap().to_string();
        assert!(agent_token.starts_with("nxa_"));

        // The agent's credential works on its own routes…
        let (s, _) = post(
            &request,
            &format!("/api/v1/agents/{agent_id}/heartbeat"),
            Some(&agent_token),
            &json!({}),
        )
        .await;
        assert_eq!(s, 200);
        let (s, _) = get(
            &request,
            &format!("/api/v1/agents/{agent_id}/environment"),
            &agent_token,
        )
        .await;
        assert_eq!(s, 200);
        let (s, _) = get(
            &request,
            &format!("/api/v1/agents/{agent_id}/tasks"),
            &agent_token,
        )
        .await;
        assert_eq!(s, 200);

        // …not on another agent's (legacy enrollment with the system key)…
        let (s, other) = post(
            &request,
            "/api/v1/agents/enroll",
            Some(&root),
            &json!({"hostname": "legacy-1"}),
        )
        .await;
        assert_eq!(s, 201);
        assert!(other["id"].as_str().is_some());
        assert!(other["agentToken"].as_str().unwrap().starts_with("nxa_"));
        let other_id = other["id"].as_str().unwrap();
        let (s, _) = post(
            &request,
            &format!("/api/v1/agents/{other_id}/heartbeat"),
            Some(&agent_token),
            &json!({}),
        )
        .await;
        assert_eq!(s, 401);
        // …and never on operator routes.
        let (s, _) = get(&request, "/api/v1/agents", &agent_token).await;
        assert_eq!(s, 401);
        let (s, _) = get(&request, "/api/v1/enrollment-tokens", &agent_token).await;
        assert_eq!(s, 401);
        let (s, _) = post(
            &request,
            "/api/v1/tasks",
            Some(&agent_token),
            &json!({"intent": "x", "targets": []}),
        )
        .await;
        assert_eq!(s, 401);

        // The token is used up.
        let (s, b) = post(
            &request,
            "/api/v1/agents/enroll",
            None,
            &json!({
                "hostname": "web-2", "enrollmentToken": plaintext,
            }),
        )
        .await;
        assert_eq!((s, b["error"].as_str()), (401, Some("unauthorized")));
        let (s, b) = post(
            &request,
            "/api/v1/agents/enroll",
            None,
            &json!({"hostname": "x"}),
        )
        .await;
        assert_eq!((s, b["error"].as_str()), (401, Some("unauthorized")));
        let (_, read) = get(
            &request,
            &format!("/api/v1/enrollment-tokens/{token_id}"),
            &root,
        )
        .await;
        assert_eq!(read["uses"], 1);
        assert_eq!(read["agents"], json!([agent_id]));
        assert!(read.get("token").is_none());

        // Re-adoption: same machine id, new token in the same hostgroup.
        let again = mint(&request, json!({"hostgroup": "acme"})).await;
        let (s, re) = post(
            &request,
            "/api/v1/agents/enroll",
            None,
            &json!({
                "hostname": "web-1-reinstalled", "machineId": "4c4c4544-0001",
                "enrollmentToken": again["token"],
            }),
        )
        .await;
        assert_eq!(s, 201);
        assert_eq!(re["agentId"], agent_id.as_str());
        assert_eq!(re["readopted"], true);
        assert_eq!(re["hostname"], "web-1-reinstalled");
        let new_token = re["agentToken"].as_str().unwrap();
        assert_ne!(new_token, agent_token);
        let (s, _) = post(
            &request,
            &format!("/api/v1/agents/{agent_id}/heartbeat"),
            Some(&agent_token),
            &json!({}),
        )
        .await;
        assert_eq!(s, 401, "the old credential was rotated out");
        let (s, _) = post(
            &request,
            &format!("/api/v1/agents/{agent_id}/heartbeat"),
            Some(new_token),
            &json!({}),
        )
        .await;
        assert_eq!(s, 200);

        // A token for another hostgroup cannot take the machine over.
        let foreign = mint(&request, json!({"hostgroup": "globex"})).await;
        let (s, b) = post(&request, "/api/v1/agents/enroll", None, &json!({
            "hostname": "web-1", "machineId": "4c4c4544-0001", "enrollmentToken": foreign["token"],
        }))
        .await;
        assert_eq!((s, b["error"].as_str()), (409, Some("conflict")));
        let (_, f) = get(
            &request,
            &format!(
                "/api/v1/enrollment-tokens/{}",
                foreign["id"].as_str().unwrap()
            ),
            &root,
        )
        .await;
        assert_eq!(f["uses"], 0, "a refused enrollment does not spend a use");

        // Revoked tokens are refused; the listing is newest first.
        let (k, v) = bearer();
        let resp = request
            .delete(&format!(
                "/api/v1/enrollment-tokens/{}",
                foreign["id"].as_str().unwrap()
            ))
            .add_header(k, v)
            .await;
        assert_eq!(resp.status_code(), 204);
        let (s, _) = post(
            &request,
            "/api/v1/agents/enroll",
            None,
            &json!({
                "hostname": "web-9", "enrollmentToken": foreign["token"],
            }),
        )
        .await;
        assert_eq!(s, 401);
        let (_, list) = get(&request, "/api/v1/enrollment-tokens", &root).await;
        let list = list.as_array().unwrap();
        assert_eq!(list[0]["id"], foreign["id"]);
        assert!(list[0]["revokedAt"].is_string());
        assert!(list.iter().all(|t| t.get("token").is_none()));
    })
    .await;
}

#[tokio::test]
#[serial]
async fn richer_facts_are_stored_and_served() {
    let _env = Env::new();
    request::<App, _, _>(|request, _ctx| async move {
        let root = root();
        let (_, a) = post(&request, "/api/v1/agents/enroll", Some(&root), &json!({"hostname": "db-1"})).await;
        let id = a["id"].as_str().unwrap();
        let token = a["agentToken"].as_str().unwrap();
        let (s, _) = post(&request, &format!("/api/v1/agents/{id}/report"), Some(token), &json!({
            "os": "Ubuntu 24.04",
            "machineId": "abc123",
            "publicIp": "203.0.113.7",
            "interfaces": [{"name": "eth0", "mac": "aa", "up": true, "addresses": ["203.0.113.7/20", "10.10.0.5/16"]}],
            "listening": [{"proto": "tcp", "address": "0.0.0.0", "port": 443, "process": "nginx"}],
            "services": [{"name": "nginx.service", "state": "active", "detail": "running", "enabled": true, "description": "web"}],
            "packages": [{"name": "nginx", "version": "1.24.0", "manager": "apt"}],
            "dnsServer": {"software": "bind9", "version": "9.18", "running": true, "zones": ["example.com"]},
        }))
        .await;
        assert_eq!(s, 200);
        // A later report that omits the facts keeps them.
        let (s, _) = post(&request, &format!("/api/v1/agents/{id}/report"), Some(token), &json!({"uptimeSeconds": 5})).await;
        assert_eq!(s, 200);

        let (_, d) = get(&request, &format!("/api/v1/agents/{id}"), &root).await;
        assert_eq!(d["machineId"], "abc123");
        assert_eq!(d["publicIp"], "203.0.113.7");
        assert_eq!(d["interfaces"][0]["name"], "eth0");
        assert_eq!(d["listening"][0]["port"], 443);
        assert_eq!(d["dnsServer"]["software"], "bind9");
        assert!(d["factsAt"].is_string());
        let (_, s) = get(&request, &format!("/api/v1/agents/{id}/services"), &root).await;
        assert_eq!(s["services"][0]["name"], "nginx.service");
        let (_, p) = get(&request, &format!("/api/v1/agents/{id}/packages"), &root).await;
        assert_eq!(p["packages"][0]["version"], "1.24.0");
        // The agent cannot read operator views.
        let (st, _) = get(&request, &format!("/api/v1/agents/{id}/services"), token).await;
        assert_eq!(st, 401);

        let (_, servers) = get(&request, "/api/v1/dns/servers", &root).await;
        let me = servers.as_array().unwrap().iter().find(|s| s["agentId"] == id).unwrap();
        assert_eq!(me["software"], "bind9");
        assert_eq!(me["zones"], 1);
        assert_eq!(me["running"], true);
    })
    .await;
}

#[tokio::test]
#[serial]
async fn accepted_tasks_are_replanned_cancelled_and_expired() {
    let mut env = Env::new();
    request::<App, _, _>(|request, ctx| async move {
        let root = root();
        let (_, a) = post(
            &request,
            "/api/v1/agents/enroll",
            Some(&root),
            &json!({"hostname": "app-1"}),
        )
        .await;
        let agent_id = a["id"].as_str().unwrap().to_string();
        let token = a["agentToken"].as_str().unwrap().to_string();

        // The Orchestrator is unreachable: tasks are kept as `accepted`.
        let mut accepted = Vec::new();
        for _ in 0..3 {
            let (s, t) = post(
                &request,
                "/api/v1/tasks",
                Some(&root),
                &json!({
                    "intent": "run_command", "targets": [agent_id], "params": {"command": "true"},
                }),
            )
            .await;
            assert_eq!(s, 200);
            assert_eq!(t["status"], "accepted");
            accepted.push(t["taskId"].as_str().unwrap().to_string());
        }
        let (_, polled) = get(
            &request,
            &format!("/api/v1/agents/{agent_id}/tasks"),
            &token,
        )
        .await;
        assert_eq!(polled, json!([]), "nothing to hand over while unplanned");

        // Cancel one; it is never handed over, and cancelling twice is 409.
        let (s, c) = post(
            &request,
            &format!("/api/v1/tasks/{}/cancel", accepted[2]),
            Some(&root),
            &json!({}),
        )
        .await;
        assert_eq!(s, 200);
        assert_eq!(c, json!({"taskId": accepted[2], "status": "cancelled"}));
        let (s, b) = post(
            &request,
            &format!("/api/v1/tasks/{}/cancel", accepted[2]),
            Some(&root),
            &json!({}),
        )
        .await;
        assert_eq!((s, b["error"].as_str()), (409, Some("conflict")));
        let (s, _) = post(
            &request,
            &format!("/api/v1/tasks/{}/cancel", uuid::Uuid::new_v4()),
            Some(&root),
            &json!({}),
        )
        .await;
        assert_eq!(s, 404);

        // One is older than the window: the sweep fails it.
        let old = tasks::Model::find_by_task_id(&ctx.db, &accepted[1].parse().unwrap())
            .await
            .unwrap();
        let mut active: tasks::ActiveModel = old.into();
        active.created_at =
            ActiveValue::set((chrono::Utc::now() - chrono::Duration::hours(25)).into());
        active.update(&ctx.db).await.unwrap();

        // The Orchestrator comes back; the agent's poll re-plans its task.
        let stub = Stub::start().await;
        stub.wire(&mut env);
        let (_, polled) = get(
            &request,
            &format!("/api/v1/agents/{agent_id}/tasks"),
            &token,
        )
        .await;
        let polled = polled.as_array().unwrap();
        assert_eq!(polled.len(), 1, "{polled:?}");
        assert_eq!(polled[0]["taskId"], accepted[0].as_str());
        assert_eq!(polled[0]["plan"]["steps"][0]["params"]["command"], "true");

        // (The poll already expired the stale one; the sweep would have too.)
        dispatch::sweep_accepted(&ctx).await.unwrap();
        let (k, v) = bearer();
        let t: Value = request
            .get(&format!("/api/v1/tasks/{}", accepted[1]))
            .add_header(k, v)
            .await
            .json();
        assert_eq!(t["status"], "failed");
        assert_eq!(t["result"]["error"], "never planned");

        // A cancelled task stays cancelled even when re-planning runs.
        let (k, v) = bearer();
        let t: Value = request
            .get(&format!("/api/v1/tasks/{}", accepted[2]))
            .add_header(k, v)
            .await
            .json();
        assert_eq!(t["status"], "cancelled");

        // The sweep plans what it can.
        env.set("LINEXUS_ORCH_URL", "http://127.0.0.1:9");
        let (_, t) = post(
            &request,
            "/api/v1/tasks",
            Some(&root),
            &json!({"intent": "run_command", "targets": [agent_id]}),
        )
        .await;
        assert_eq!(t["status"], "accepted");
        stub.wire(&mut env);
        let report = dispatch::sweep_accepted(&ctx).await.unwrap();
        assert_eq!(report.planned, 1);

        // An agent cannot report on someone else's task.
        let (s, _) = post(
            &request,
            &format!(
                "/api/v1/agents/{agent_id}/tasks/{}/result",
                t["taskId"].as_str().unwrap()
            ),
            Some(&token),
            &json!({"status": "success"}),
        )
        .await;
        assert_eq!(s, 200, "its own task");
        let (_, other) = post(
            &request,
            "/api/v1/agents/enroll",
            Some(&root),
            &json!({"hostname": "app-2"}),
        )
        .await;
        let other_token = other["agentToken"].as_str().unwrap();
        let other_id = other["id"].as_str().unwrap();
        let (s, _) = post(
            &request,
            &format!("/api/v1/agents/{other_id}/tasks/{}/result", accepted[0]),
            Some(other_token),
            &json!({"status": "success"}),
        )
        .await;
        assert_eq!(s, 404);
    })
    .await;
}

#[tokio::test]
#[serial]
async fn hostgroup_targets_expand_at_creation() {
    let mut env = Env::new();
    let stub = Stub::start().await;
    stub.wire(&mut env);
    request::<App, _, _>(|request, _ctx| async move {
        let root = root();
        let mut ids = Vec::new();
        for h in ["hg-a", "hg-b"] {
            let (_, a) = post(
                &request,
                "/api/v1/agents/enroll",
                Some(&root),
                &json!({"hostname": h, "hostgroup": "blue"}),
            )
            .await;
            ids.push(a["id"].as_str().unwrap().to_string());
        }
        post(
            &request,
            "/api/v1/agents/enroll",
            Some(&root),
            &json!({"hostname": "hg-c", "hostgroup": "green"}),
        )
        .await;
        let (_, t) = post(
            &request,
            "/api/v1/tasks",
            Some(&root),
            &json!({
                "intent": "run_command", "targets": ["hostgroup:blue", ids[0]],
            }),
        )
        .await;
        assert_eq!(t["status"], "planned");
        let (_, task) = get(
            &request,
            &format!("/api/v1/tasks/{}", t["taskId"].as_str().unwrap()),
            &root,
        )
        .await;
        assert_eq!(task["targets"], json!(ids));
        let plan = stub.seen("POST", "/orch/plan");
        assert_eq!(plan.last().unwrap().body["targets"], json!(ids));
    })
    .await;
}
