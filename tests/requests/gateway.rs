//! The Hub-facing task surface: `GET /api/v1/tasks/{id}` and the agent's
//! result report that feeds it.
//!
//! Tasks are created straight in the database in the `planned` state, as a
//! successful Orchestrator call would leave them, so these tests do not depend
//! on an Orchestrator being reachable. The Logger is not running either; the
//! gateway's audit-log calls fail and are logged, which never fails a request.

use axum::http::{HeaderName, HeaderValue};
use linexus_nexus::{
    app::App,
    middleware::system_token,
    models::tasks::{self, CreateTaskParams, OUTPUT_CAP},
};
use loco_rs::{app::AppContext, testing::prelude::*, TestServer};
use serde_json::{json, Value};
use serial_test::serial;

fn bearer() -> (HeaderName, HeaderValue) {
    let token = std::env::var(system_token::ENV_ROOT_TOKEN)
        .unwrap_or_else(|_| system_token::DEV_ROOT_TOKEN.to_string());
    (
        HeaderName::from_static("authorization"),
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    )
}

/// A planned task targeting one fresh agent id; returns `(task_id, agent_id)`.
async fn planned_task(ctx: &AppContext, intent: &str) -> (String, String) {
    let agent_id = uuid::Uuid::new_v4().to_string();
    let params = CreateTaskParams {
        intent: intent.to_string(),
        target_agents: Some(vec![agent_id.clone()]),
    };
    let task = tasks::Model::create(&ctx.db, "service:test", &params)
        .await
        .unwrap();
    let plan = json!({
        "task_id": task.task_id.to_string(),
        "intent": intent,
        "targets": [agent_id],
        "auto_rollback": false,
        "steps": [],
    });
    let task = task
        .set_plan(&ctx.db, &plan.to_string(), "planned")
        .await
        .unwrap();
    (task.task_id.to_string(), agent_id)
}

async fn get_task(request: &TestServer, task_id: &str) -> (u16, Value) {
    let (k, v) = bearer();
    let response = request
        .get(&format!("/api/v1/tasks/{task_id}"))
        .add_header(k, v)
        .await;
    let status = response.status_code().as_u16();
    (status, serde_json::from_str(&response.text()).unwrap())
}

async fn post_result(request: &TestServer, agent_id: &str, task_id: &str, body: &Value) -> u16 {
    let (k, v) = bearer();
    request
        .post(&format!("/api/v1/agents/{agent_id}/tasks/{task_id}/result"))
        .add_header(k, v)
        .json(body)
        .await
        .status_code()
        .as_u16()
}

#[tokio::test]
#[serial]
async fn get_task_unknown_id_is_404() {
    request::<App, _, _>(|request, _ctx| async move {
        let (status, body) = get_task(&request, &uuid::Uuid::new_v4().to_string()).await;
        assert_eq!(status, 404);
        assert_eq!(body, json!({ "error": "not_found" }));

        // Not a UUID: it cannot name a task either.
        let (status, body) = get_task(&request, "not-a-task").await;
        assert_eq!(status, 404);
        assert_eq!(body, json!({ "error": "not_found" }));
    })
    .await;
}

#[tokio::test]
#[serial]
async fn get_task_requires_bearer() {
    request::<App, _, _>(|request, ctx| async move {
        let (task_id, _) = planned_task(&ctx, "run_command").await;
        let response = request.get(&format!("/api/v1/tasks/{task_id}")).await;
        assert_eq!(response.status_code(), 401);
    })
    .await;
}

#[tokio::test]
#[serial]
async fn get_task_follows_lifecycle_to_result_with_steps() {
    request::<App, _, _>(|request, ctx| async move {
        let (task_id, agent_id) = planned_task(&ctx, "deploy_file").await;

        // Planned: no result yet.
        let (status, body) = get_task(&request, &task_id).await;
        assert_eq!(status, 200);
        assert_eq!(body["taskId"], task_id);
        assert_eq!(body["intent"], "deploy_file");
        assert_eq!(body["status"], "planned");
        assert_eq!(body["targets"], json!([agent_id]));
        assert!(body["createdAt"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(body["updatedAt"].as_str().is_some_and(|s| !s.is_empty()));
        assert_eq!(body["completedAt"], "");
        assert_eq!(body["result"], Value::Null);

        // The agent polls: the task is handed over and becomes dispatched.
        let (k, v) = bearer();
        let polled = request
            .get(&format!("/api/v1/agents/{agent_id}/tasks"))
            .add_header(k, v)
            .await;
        assert_eq!(polled.status_code(), 200);
        let (_, body) = get_task(&request, &task_id).await;
        assert_eq!(body["status"], "dispatched");
        assert_eq!(body["result"], Value::Null);

        // The agent reports a failure with per-step results.
        let report = json!({
            "status": "failed",
            "error": "exit status 3",
            "message": "executed deploy_file (3 steps)",
            "exitCode": 3,
            "output": "==> file.write [success]\nwritten\n==> command.run [failed]\nboom",
            "steps": [
                { "id": "s1", "action": "file.write", "status": "success",
                  "changed": true, "output": "written", "error": "" },
                { "id": "s2", "action": "command.run", "status": "failed",
                  "changed": true, "output": "boom", "error": "exit status 3",
                  "name": "run the deploy hook", "exitCode": 3 },
                { "id": "s3", "action": "service.ensure", "status": "skipped" },
            ],
        });
        assert_eq!(
            post_result(&request, &agent_id, &task_id, &report).await,
            200
        );

        let (status, body) = get_task(&request, &task_id).await;
        assert_eq!(status, 200);
        assert_eq!(body["status"], "failed");
        assert!(body["completedAt"].as_str().is_some_and(|s| !s.is_empty()));
        let result = &body["result"];
        assert_eq!(result["status"], "failed");
        assert_eq!(result["exitCode"], 3);
        assert_eq!(result["message"], "executed deploy_file (3 steps)");
        assert_eq!(result["error"], "exit status 3");
        assert_eq!(result["output"], report["output"]);
        assert_eq!(
            result["steps"],
            // `name` is the step's own (else its action) and `exitCode` as
            // reported (else 0 / 1 from the status; null when it never ran).
            json!([
                { "id": "s1", "name": "file.write", "action": "file.write",
                  "status": "success", "exitCode": 0,
                  "changed": true, "output": "written", "error": "" },
                { "id": "s2", "name": "run the deploy hook", "action": "command.run",
                  "status": "failed", "exitCode": 3,
                  "changed": true, "output": "boom", "error": "exit status 3" },
                { "id": "s3", "name": "service.ensure", "action": "service.ensure",
                  "status": "skipped", "exitCode": null,
                  "changed": false, "output": "", "error": "" },
            ])
        );
    })
    .await;
}

#[tokio::test]
#[serial]
async fn legacy_result_body_still_accepted() {
    request::<App, _, _>(|request, ctx| async move {
        let (task_id, agent_id) = planned_task(&ctx, "run_command").await;

        // An agent that predates exitCode/output/steps.
        let report = json!({ "status": "success", "message": "executed run_command (1 steps)" });
        assert_eq!(
            post_result(&request, &agent_id, &task_id, &report).await,
            200
        );

        let (_, body) = get_task(&request, &task_id).await;
        assert_eq!(body["status"], "completed");
        let result = &body["result"];
        assert_eq!(result["status"], "success");
        assert_eq!(result["exitCode"], 0);
        assert_eq!(result["output"], "");
        assert_eq!(result["steps"], json!([]));
    })
    .await;
}

#[tokio::test]
#[serial]
async fn steps_without_output_fill_the_output() {
    request::<App, _, _>(|request, ctx| async move {
        let (task_id, agent_id) = planned_task(&ctx, "run_command").await;
        let report = json!({
            "status": "success",
            "steps": [{ "action": "command.run", "status": "success", "changed": true, "output": "hello" }],
        });
        assert_eq!(post_result(&request, &agent_id, &task_id, &report).await, 200);

        let (_, body) = get_task(&request, &task_id).await;
        assert_eq!(body["result"]["output"], "==> command.run [success]\nhello");
    })
    .await;
}

#[tokio::test]
#[serial]
async fn result_output_is_truncated_keeping_the_tail() {
    request::<App, _, _>(|request, ctx| async move {
        let (task_id, agent_id) = planned_task(&ctx, "run_command").await;

        let big = format!("{}THE-END", "x".repeat(100 * 1024));
        let step_big = format!("{}STEP-END", "y".repeat(40 * 1024));
        let report = json!({
            "status": "success",
            "exitCode": 0,
            "output": big,
            "steps": [{ "action": "command.run", "status": "success", "changed": true, "output": step_big }],
        });
        assert_eq!(post_result(&request, &agent_id, &task_id, &report).await, 200);

        let (_, body) = get_task(&request, &task_id).await;
        let output = body["result"]["output"].as_str().unwrap();
        assert!(output.len() <= OUTPUT_CAP, "stored output is capped");
        assert!(output.starts_with("[... truncated "), "marker leads the output");
        assert!(output.ends_with("THE-END"), "the tail is kept");

        let step_output = body["result"]["steps"][0]["output"].as_str().unwrap();
        assert!(step_output.len() <= tasks::STEP_OUTPUT_CAP);
        assert!(step_output.starts_with("[... truncated "));
        assert!(step_output.ends_with("STEP-END"));
    })
    .await;
}

#[test]
fn cap_output_respects_char_boundaries() {
    let s = "é".repeat(100); // 200 bytes, 2 per char
    let capped = tasks::cap_output(&s, 101);
    assert!(capped.len() <= 101);
    assert!(capped.starts_with("[... truncated "));
    assert!(capped.ends_with('é'));
    assert_eq!(tasks::cap_output("short", 101), "short");
}

#[tokio::test]
#[serial]
async fn result_is_forwarded_to_the_hub_with_exit_code_and_output() {
    use std::sync::{Arc, Mutex};

    // A stand-in for the Hub's automation results ingest.
    let received: Arc<Mutex<Vec<Value>>> = Arc::default();
    let sink = received.clone();
    let hub = axum::Router::new().route(
        "/api/v1/automation/results/task",
        axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
            let sink = sink.clone();
            async move {
                sink.lock().unwrap().push(body);
                "ok"
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, hub).await.unwrap() });
    std::env::set_var("DAEDALUS_INGEST_URL", format!("http://{addr}"));

    request::<App, _, _>(|request, ctx| async move {
        // With steps and an exit code but no combined output: the Hub gets the
        // agent's exit code and the steps' output.
        let (task_id, agent_id) = planned_task(&ctx, "run_command").await;
        let report = json!({
            "status": "failed",
            "error": "exit status 7",
            "exitCode": 7,
            "steps": [{ "action": "command.run", "status": "failed", "changed": true,
                        "output": "no space left", "error": "exit status 7" }],
        });
        assert_eq!(
            post_result(&request, &agent_id, &task_id, &report).await,
            200
        );

        // A legacy body: exit code derived, output falls back to the message.
        let (legacy_id, legacy_agent) = planned_task(&ctx, "run_command").await;
        let legacy = json!({ "status": "success", "message": "executed run_command (1 steps)" });
        assert_eq!(
            post_result(&request, &legacy_agent, &legacy_id, &legacy).await,
            200
        );

        let got = received.lock().unwrap().clone();
        assert_eq!(
            got,
            vec![
                json!({
                    "linexusTaskId": task_id,
                    "status": "failed",
                    "exitCode": 7,
                    "output": "==> command.run [failed]\nno space left\nerror: exit status 7",
                }),
                json!({
                    "linexusTaskId": legacy_id,
                    "status": "success",
                    "exitCode": 0,
                    "output": "executed run_command (1 steps)",
                }),
            ]
        );
    })
    .await;

    std::env::remove_var("DAEDALUS_INGEST_URL");
}
