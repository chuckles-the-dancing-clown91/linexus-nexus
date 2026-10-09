//! Production hardening (`docs/PROVIDERS.md` §11–12): signed plans, operator
//! scopes, the operator client-certificate rule, provider credentials
//! bootstrapped from the environment, snapshot deletion, certificates and the
//! production configuration.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use linexus_nexus::{
    app::App,
    middleware::client_cert,
    models::{enrollment_tokens, provider_credentials, system_tokens, tasks},
    providers, secrets,
    signing::{self, Signer},
};
use loco_rs::{app::AppContext, environment::Environment, testing::prelude::*, TestServer};
use serde_json::{json, Value};
use serial_test::serial;
use sha2::{Digest, Sha256};

use super::stub::{bearer, bearer_of, header, Env, Stub, CF_TOKEN, DO_TOKEN};

/// Send a request with `token` as the bearer (none when `None`).
async fn send(
    request: &TestServer,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
    extra: &[(&'static str, &str)],
) -> (u16, Value) {
    let mut req = match method {
        "GET" => request.get(path),
        "POST" => request.post(path),
        "PUT" => request.put(path),
        "DELETE" => request.delete(path),
        _ => unreachable!(),
    };
    if let Some(t) = token {
        let (k, v) = bearer_of(t);
        req = req.add_header(k, v);
    }
    for (name, value) in extra {
        let (k, v) = header(name, value);
        req = req.add_header(k, v);
    }
    if let Some(b) = body {
        req = req.json(&b);
    }
    let resp = req.await;
    (
        resp.status_code().as_u16(),
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

/// A planned task for `agent_id` carrying `plan`, as the Orchestrator would
/// leave it.
async fn planned_task(ctx: &AppContext, agent_id: &str, plan: &Value) -> String {
    let task = tasks::Model::create(
        &ctx.db,
        "service:test",
        &tasks::CreateTaskParams {
            intent: "run_command".into(),
            target_agents: Some(vec![agent_id.to_string()]),
        },
    )
    .await
    .unwrap();
    let id = task.task_id.to_string();
    task.set_plan(&ctx.db, &plan.to_string(), "planned")
        .await
        .unwrap();
    id
}

fn decode_key(b64: &str) -> VerifyingKey {
    let raw = B64.decode(b64).unwrap();
    VerifyingKey::from_bytes(&<[u8; 32]>::try_from(raw.as_slice()).unwrap()).unwrap()
}

/// Verify an envelope with `key` and return its decoded payload.
fn verified_payload(envelope: &Value, key: &VerifyingKey) -> (Vec<u8>, Value) {
    assert_eq!(envelope["alg"], "ed25519");
    let payload = B64.decode(envelope["payload"].as_str().unwrap()).unwrap();
    let sig = B64.decode(envelope["signature"].as_str().unwrap()).unwrap();
    assert_eq!(sig.len(), 64);
    let sig = Signature::from_bytes(&<[u8; 64]>::try_from(sig.as_slice()).unwrap());
    key.verify(&payload, &sig).expect("the envelope verifies");
    let parsed = serde_json::from_slice(&payload).unwrap();
    (payload, parsed)
}

#[tokio::test]
#[serial]
async fn signed_plans_verify_with_the_published_key() {
    let mut env = Env::new();
    env.set("NEXUS_PLAN_TTL_SECS", "600");
    request::<App, _, _>(|request, ctx| async move {
        let (s, key) = send(&request, "GET", "/api/v1/signing-key", None, None, &[]).await;
        assert_eq!(s, 200);
        assert_eq!(key["alg"], "ed25519");
        let public = decode_key(key["publicKey"].as_str().unwrap());
        let expected_id = hex::encode(Sha256::digest(public.as_bytes()));
        assert_eq!(key["keyId"], &expected_id[..16]);

        // Enrollment hands the agent the same key to pin.
        let (s, agent) = send(&request, "POST", "/api/v1/agents/enroll", Some(&root()), Some(json!({"hostname": "web-1"})), &[]).await;
        assert_eq!(s, 201, "{agent}");
        assert_eq!(agent["signingKey"], key);
        let agent_id = agent["agentId"].as_str().unwrap().to_string();
        let agent_token = agent["agentToken"].as_str().unwrap().to_string();

        let plan = json!({
            "task_id": "x", "intent": "run_command", "auto_rollback": true,
            "steps": [{"id": "s1", "action": "command.run", "params": {"cmd": "uptime"}, "critical": true}],
        });
        let task_id = planned_task(&ctx, &agent_id, &plan).await;

        let (s, polled) = send(&request, "GET", &format!("/api/v1/agents/{agent_id}/tasks"), Some(&agent_token), None, &[]).await;
        assert_eq!(s, 200, "{polled}");
        let t = &polled[0];
        // Every field the task had is still there…
        assert_eq!(t["taskId"], task_id.as_str());
        assert_eq!(t["intent"], "run_command");
        assert_eq!(t["status"], "planned");
        assert_eq!(t["autoRollback"], true);
        assert_eq!(t["plan"], plan);
        // …plus an envelope over exactly that plan.
        assert_eq!(t["envelope"]["keyId"], key["keyId"]);
        let (_, payload) = verified_payload(&t["envelope"], &public);
        let mut keys: Vec<&str> = payload.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["agentId", "expiresAt", "intent", "issuedAt", "nonce", "plan", "taskId", "v"]);
        assert_eq!(payload["v"], 1);
        assert_eq!(payload["taskId"], task_id.as_str());
        assert_eq!(payload["agentId"], agent_id.as_str());
        assert_eq!(payload["intent"], "run_command");
        assert_eq!(payload["plan"], t["plan"]);
        let issued = chrono::DateTime::parse_from_rfc3339(payload["issuedAt"].as_str().unwrap()).unwrap();
        let expires = chrono::DateTime::parse_from_rfc3339(payload["expiresAt"].as_str().unwrap()).unwrap();
        assert_eq!((expires - issued).num_seconds(), 600);
        assert!(payload["issuedAt"].as_str().unwrap().ends_with('Z'));
        assert!((chrono::Utc::now() - issued.with_timezone(&chrono::Utc)).num_seconds().abs() < 60);
        let nonce = payload["nonce"].as_str().unwrap();
        assert!(nonce.len() == 32 && nonce.chars().all(|c| c.is_ascii_hexdigit()));

        // Each delivery is signed afresh.
        let (_, again) = send(&request, "GET", &format!("/api/v1/agents/{agent_id}/tasks"), Some(&agent_token), None, &[]).await;
        let (_, payload2) = verified_payload(&again[0]["envelope"], &public);
        assert_eq!(payload2["taskId"], task_id.as_str());
        assert_ne!(payload2["nonce"], payload["nonce"]);

        // A system key polling for an agent gets envelopes for that agent.
        let (s, by_root) = send(&request, "GET", &format!("/api/v1/agents/{agent_id}/tasks"), Some(&root()), None, &[]).await;
        assert_eq!(s, 200);
        let (_, payload3) = verified_payload(&by_root[0]["envelope"], &public);
        assert_eq!(payload3["agentId"], agent_id.as_str());

        // The enrollment-token path carries the key too.
        let (row, plaintext) = enrollment_tokens::Model::mint(&ctx.db, &enrollment_tokens::MintParams {
            hostgroup: "acme".into(), environment: "production".into(), label: None,
            metadata: None, ttl_minutes: 60, max_uses: 1, created_by: None,
        }).await.unwrap();
        let (s, enrolled) = send(&request, "POST", "/api/v1/agents/enroll", None, Some(json!({"hostname": "db-1", "enrollmentToken": plaintext})), &[]).await;
        assert_eq!(s, 201, "{enrolled}");
        assert_eq!(enrolled["enrollmentTokenId"], row.token_id.to_string());
        assert_eq!(enrolled["signingKey"], key);
    })
    .await;
}

#[tokio::test]
#[serial]
async fn signing_key_is_persisted_and_the_environment_rotates_it() {
    let mut env = Env::new();
    request::<App, _, _>(|request, ctx| async move {
        let booted = signing::signer(&ctx).await.unwrap();
        // A restart reads the same key back out of the database.
        let _ = ctx.shared_store.remove::<Signer>();
        let restarted = signing::init(&ctx).await.unwrap();
        assert_eq!(restarted.key_id(), booted.key_id());
        assert_eq!(restarted.public_key_b64(), booted.public_key_b64());

        // NEXUS_SIGNING_KEY wins, and replaces what is stored.
        let seed = [9u8; 32];
        env.set(signing::ENV_SIGNING_KEY, &B64.encode(seed));
        let rotated = signing::init(&ctx).await.unwrap();
        assert_eq!(rotated.key_id(), Signer::from_seed(&seed).key_id());
        assert_ne!(rotated.key_id(), booted.key_id());
        env.unset(signing::ENV_SIGNING_KEY);
        let _ = ctx.shared_store.remove::<Signer>();
        let after = signing::init(&ctx).await.unwrap();
        assert_eq!(after.key_id(), rotated.key_id());

        let (_, key) = send(&request, "GET", "/api/v1/signing-key", None, None, &[]).await;
        assert_eq!(key["keyId"], rotated.key_id());

        // A malformed key refuses to start.
        env.set(signing::ENV_SIGNING_KEY, "not-a-key");
        assert!(signing::init(&ctx).await.is_err());
        env.unset(signing::ENV_SIGNING_KEY);
    })
    .await;
}

#[tokio::test]
#[serial]
async fn operator_scopes_are_enforced_on_v1() {
    let mut env = Env::new();
    let stub = Stub::start().await;
    stub.wire(&mut env);
    env.set("DIGITALOCEAN_TOKEN", DO_TOKEN);
    env.set("NEXUS_PUBLIC_URL", "https://nexus.example.com");
    request::<App, _, _>(|request, ctx| async move {
        let issue = |scopes: &'static str| {
            let db = ctx.db.clone();
            async move { system_tokens::Model::issue(&db, "hub", scopes).await.unwrap().1 }
        };
        let reader = issue("agents:read,tasks:read,infra:read").await;
        let infra = issue("infra:*").await;
        let enroller = issue("enroll").await;
        let star = issue("*").await;
        // A token minted with the publisher defaults has no /api/v1 scope.
        let publisher = issue("nodes:create,nodes:read,wallet:read").await;

        let forbidden = |scope: &str| json!({"error": "forbidden", "detail": format!("token lacks scope {scope}")});
        let call = |method: &'static str, path: &'static str, token: String, body: Option<Value>| {
            let request = &request;
            async move { send(request, method, path, Some(&token), body, &[]).await }
        };

        assert_eq!(call("GET", "/api/v1/agents", reader.clone(), None).await.0, 200);
        assert_eq!(call("GET", "/api/v1/cloud/droplets", reader.clone(), None).await.0, 200);
        assert_eq!(call("GET", "/api/v1/operations", reader.clone(), None).await.0, 200);
        let task_body = json!({"intent": "run_command", "targets": []});
        assert_eq!(call("POST", "/api/v1/tasks", reader.clone(), Some(task_body.clone())).await, (403, forbidden("tasks:write")));
        assert_eq!(call("DELETE", "/api/v1/cloud/droplets/1", reader.clone(), None).await, (403, forbidden("infra:write")));
        assert_eq!(call("PUT", "/api/v1/providers/cloudflare/credentials", reader.clone(), Some(json!({"token": "x"}))).await, (403, forbidden("infra:write")));
        assert_eq!(call("POST", "/api/v1/enrollment-tokens", reader.clone(), Some(json!({"hostgroup": "acme"}))).await, (403, forbidden("enroll")));
        assert_eq!(call("GET", "/api/v1/enrollment-tokens", reader.clone(), None).await, (403, forbidden("enroll")));
        // Adopting a machine with a system key needs agents:write.
        assert_eq!(call("POST", "/api/v1/agents/enroll", reader.clone(), Some(json!({"hostname": "h"}))).await, (403, forbidden("agents:write")));

        // A prefix grant covers its scopes and nothing else.
        assert_eq!(call("GET", "/api/v1/providers", infra.clone(), None).await.0, 200);
        assert_eq!(call("GET", "/api/v1/agents", infra.clone(), None).await, (403, forbidden("agents:read")));
        // Creating a droplet that enrolls an agent also mints a token.
        let droplet = json!({"name": "web-1", "region": "fra1", "size": "s-1vcpu-1gb", "image": "ubuntu-24-04-x64", "enrollAgent": {"hostgroup": "acme"}});
        assert_eq!(call("POST", "/api/v1/cloud/droplets", infra.clone(), Some(droplet.clone())).await, (403, forbidden("enroll")));
        assert!(stub.seen("POST", "/v2/droplets").is_empty());

        let (s, minted) = call("POST", "/api/v1/enrollment-tokens", enroller.clone(), Some(json!({"hostgroup": "acme"}))).await;
        assert_eq!(s, 201, "{minted}");
        assert_eq!(call("GET", "/api/v1/agents", enroller.clone(), None).await.0, 403);

        assert_eq!(call("GET", "/api/v1/agents", publisher.clone(), None).await, (403, forbidden("agents:read")));

        // `*` and the root token hold every scope.
        let (s, agent) = call("POST", "/api/v1/agents/enroll", star.clone(), Some(json!({"hostname": "h1"}))).await;
        assert_eq!(s, 201, "{agent}");
        assert_eq!(call("POST", "/api/v1/tasks", star.clone(), Some(task_body.clone())).await.0, 200);
        assert_eq!(call("POST", "/api/v1/cloud/droplets", star.clone(), Some(droplet)).await.0, 201);
        assert_eq!(call("POST", "/api/v1/tasks", root(), Some(task_body)).await.0, 200);

        // Agent credentials keep their narrow access, unaffected by scopes.
        let agent_id = agent["agentId"].as_str().unwrap().to_string();
        let agent_token = agent["agentToken"].as_str().unwrap().to_string();
        let (s, _) = send(&request, "POST", &format!("/api/v1/agents/{agent_id}/heartbeat"), Some(&agent_token), None, &[]).await;
        assert_eq!(s, 200);
        let (s, _) = send(&request, "GET", &format!("/api/v1/agents/{agent_id}/tasks"), Some(&agent_token), None, &[]).await;
        assert_eq!(s, 200);
        let (s, b) = call("GET", "/api/v1/agents", agent_token.clone(), None).await;
        assert_eq!((s, b["error"].as_str()), (401, Some("unauthorized")));
        // A system key on an agent route needs the matching scope.
        let (s, b) = send(&request, "POST", &format!("/api/v1/agents/{agent_id}/heartbeat"), Some(&reader), None, &[]).await;
        assert_eq!((s, b), (403, forbidden("agents:write")));
        let (s, _) = send(&request, "GET", &format!("/api/v1/agents/{agent_id}/tasks"), Some(&reader), None, &[]).await;
        assert_eq!(s, 200);
    })
    .await;
}

#[tokio::test]
#[serial]
async fn operators_need_a_client_certificate_when_required() {
    let mut env = Env::new();
    env.set(client_cert::ENV_REQUIRE, "1");
    request::<App, _, _>(|request, ctx| async move {
        // No TLS here, so no request carries a verified certificate.
        let (s, b) = send(&request, "GET", "/api/v1/agents", Some(&root()), None, &[]).await;
        assert_eq!(s, 401);
        assert_eq!(
            b,
            json!({"error": "unauthorized", "detail": client_cert::REFUSAL})
        );
        let (s, _) = send(
            &request,
            "POST",
            "/api/v1/agents/enroll",
            Some(&root()),
            Some(json!({"hostname": "h"})),
            &[],
        )
        .await;
        assert_eq!(s, 401);

        // Public routes, enrollment with a token and agents are unaffected.
        assert_eq!(
            send(&request, "GET", "/api/v1/signing-key", None, None, &[])
                .await
                .0,
            200
        );
        assert_eq!(request.get("/install/agent.sh").await.status_code(), 200);
        let (_, plaintext) = enrollment_tokens::Model::mint(
            &ctx.db,
            &enrollment_tokens::MintParams {
                hostgroup: "acme".into(),
                environment: "production".into(),
                label: None,
                metadata: None,
                ttl_minutes: 60,
                max_uses: 1,
                created_by: None,
            },
        )
        .await
        .unwrap();
        let (s, agent) = send(
            &request,
            "POST",
            "/api/v1/agents/enroll",
            None,
            Some(json!({"hostname": "h", "enrollmentToken": plaintext})),
            &[],
        )
        .await;
        assert_eq!(s, 201, "{agent}");
        let agent_id = agent["agentId"].as_str().unwrap();
        let token = agent["agentToken"].as_str().unwrap();
        let (s, _) = send(
            &request,
            "POST",
            &format!("/api/v1/agents/{agent_id}/heartbeat"),
            Some(token),
            None,
            &[],
        )
        .await;
        assert_eq!(s, 200);

        // An empty value (as Compose passes it) is off.
        env.set(client_cert::ENV_REQUIRE, "");
        assert_eq!(
            send(&request, "GET", "/api/v1/agents", Some(&root()), None, &[])
                .await
                .0,
            200
        );
    })
    .await;
}

#[tokio::test]
#[serial]
async fn provider_credentials_bootstrap_from_the_environment() {
    let mut env = Env::new();
    env.set("DIGITALOCEAN_TOKEN", DO_TOKEN);
    env.set("CLOUDFLARE_API_TOKEN", CF_TOKEN);
    env.set("CLOUDFLARE_ACCOUNT_ID", "acc1");
    request::<App, _, _>(|request, ctx| async move {
        let unseal = |row: &provider_credentials::Model| {
            secrets::open(
                &ctx.environment,
                &row.provider,
                row.sealed_token.as_deref().unwrap(),
            )
            .unwrap()
        };
        let row = provider_credentials::Model::find_by_provider(&ctx.db, "digitalocean")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(unseal(&row), DO_TOKEN);
        assert!(!row
            .sealed_token
            .as_ref()
            .unwrap()
            .windows(8)
            .any(|w| DO_TOKEN.as_bytes().starts_with(w)));
        let row = provider_credentials::Model::find_by_provider(&ctx.db, "cloudflare")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(unseal(&row), CF_TOKEN);
        assert_eq!(row.account_id.as_deref(), Some("acc1"));

        let (s, ops) = send(
            &request,
            "GET",
            "/api/v1/operations",
            Some(&root()),
            None,
            &[],
        )
        .await;
        assert_eq!(s, 200);
        let boot: Vec<&Value> = ops
            .as_array()
            .unwrap()
            .iter()
            .filter(|o| o["requester"] == providers::BOOTSTRAP_REQUESTER)
            .collect();
        assert_eq!(boot.len(), 2, "{ops}");
        assert!(boot
            .iter()
            .all(|o| o["operation"] == "credentials.put" && o["status"] == "ok"));

        // Stored credentials are never overwritten.
        let other = secrets::seal(&ctx.environment, "digitalocean", "dop_v1_other").unwrap();
        provider_credentials::Model::store(&ctx.db, "digitalocean", other, None)
            .await
            .unwrap();
        assert!(providers::bootstrap_from_env(&ctx).await.is_empty());
        let row = provider_credentials::Model::find_by_provider(&ctx.db, "digitalocean")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(unseal(&row), "dop_v1_other");
        let (_, ops) = send(
            &request,
            "GET",
            "/api/v1/operations",
            Some(&root()),
            None,
            &[],
        )
        .await;
        assert_eq!(ops.as_array().unwrap().len(), 2);

        // Empty variables (as Compose passes them) bootstrap nothing.
        provider_credentials::Model::clear(&ctx.db, "cloudflare")
            .await
            .unwrap();
        env.set("CLOUDFLARE_API_TOKEN", "");
        assert!(providers::bootstrap_from_env(&ctx).await.is_empty());
        assert!(
            provider_credentials::Model::find_by_provider(&ctx.db, "cloudflare")
                .await
                .unwrap()
                .unwrap()
                .sealed_token
                .is_none()
        );

        // Credentials deleted through the API come back on the next start.
        provider_credentials::Model::clear(&ctx.db, "digitalocean")
            .await
            .unwrap();
        assert_eq!(
            providers::bootstrap_from_env(&ctx).await,
            vec!["digitalocean"]
        );
    })
    .await;
}

#[tokio::test]
#[serial]
async fn snapshot_deletion_needs_confirmation_and_certificates_are_listed() {
    let mut env = Env::new();
    let stub = Stub::start().await;
    stub.wire(&mut env);
    env.set("DIGITALOCEAN_TOKEN", DO_TOKEN);
    stub.state.lock().unwrap().snapshots.push(json!({
        "id": "6372321", "name": "web-1-before-upgrade", "resource_type": "droplet",
        "regions": ["fra1"], "size_gigabytes": 2.4, "created_at": "2026-10-01T00:00:00Z",
    }));
    request::<App, _, _>(|request, _ctx| async move {
        let path = "/api/v1/cloud/snapshots/6372321";
        let (s, b) = send(&request, "DELETE", path, Some(&root()), None, &[]).await;
        assert_eq!(
            (s, b["error"].as_str()),
            (412, Some("confirmation_required"))
        );
        let (s, _) = send(
            &request,
            "DELETE",
            path,
            Some(&root()),
            None,
            &[("x-confirm", "web-1")],
        )
        .await;
        assert_eq!(s, 412);
        assert!(stub.seen("DELETE", "/v2/snapshots/6372321").is_empty());
        let (s, _) = send(
            &request,
            "DELETE",
            path,
            Some(&root()),
            None,
            &[("x-confirm", "web-1-before-upgrade")],
        )
        .await;
        assert_eq!(s, 204);
        assert_eq!(stub.seen("DELETE", "/v2/snapshots/6372321").len(), 1);
        let (s, _) = send(
            &request,
            "DELETE",
            path,
            Some(&root()),
            None,
            &[("x-confirm", "web-1-before-upgrade")],
        )
        .await;
        assert_eq!(s, 404);
        let (s, _) = send(
            &request,
            "DELETE",
            "/api/v1/cloud/snapshots/..%2Fdroplets",
            Some(&root()),
            None,
            &[],
        )
        .await;
        assert_eq!(s, 404);

        let (_, ops) = send(
            &request,
            "GET",
            "/api/v1/operations?provider=digitalocean",
            Some(&root()),
            None,
            &[],
        )
        .await;
        let op = ops
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["operation"] == "snapshot.delete")
            .unwrap();
        assert_eq!(op["status"], "ok");
        assert_eq!(op["target"], "6372321 web-1-before-upgrade");

        let (s, certs) = send(
            &request,
            "GET",
            "/api/v1/cloud/certificates",
            Some(&root()),
            None,
            &[],
        )
        .await;
        assert_eq!(s, 200);
        assert_eq!(
            certs,
            json!([{
                "id": "892071a0-bb95-49bc-8021-3afd67a210bf", "name": "web-cert-01",
                "type": "lets_encrypt", "dnsNames": ["www.example.com", "example.com"],
                "notAfter": "2027-02-22T00:23:00Z", "state": "verified",
            }])
        );
    })
    .await;
}

#[tokio::test]
#[serial]
async fn production_config_comes_from_the_environment() {
    let mut env = Env::new();
    let load = || {
        loco_rs::config::Config::from_folder(
            &Environment::Production,
            std::path::Path::new("config"),
        )
        .map(|c| serde_json::to_value(&c).unwrap())
    };
    env.unset("DATABASE_URL");
    env.set(
        "NEXUS_JWT_SECRET",
        "a-production-jwt-secret-of-enough-length",
    );
    assert!(load().is_err(), "DATABASE_URL is required");

    env.set("DATABASE_URL", "postgres://nexus:pw@db:5432/nexus");
    env.set("PORT", "8443");
    env.set(
        "NEXUS_CORS_ORIGINS",
        " https://hub.example.com, https://ops.example.com ,",
    );
    let c = load().unwrap();
    assert_eq!(c["server"]["port"], 8443);
    assert_eq!(c["server"]["binding"], "0.0.0.0");
    assert_eq!(c["logger"]["format"], "json");
    assert_eq!(c["logger"]["level"], "info");
    assert_eq!(c["database"]["uri"], "postgres://nexus:pw@db:5432/nexus");
    assert_eq!(c["database"]["dangerously_recreate"], false);
    assert_eq!(
        c["auth"]["jwt"]["secret"],
        "a-production-jwt-secret-of-enough-length"
    );
    let cors = &c["server"]["middlewares"]["cors"];
    assert_eq!(cors["enable"], true);
    assert_eq!(
        cors["allow_origins"],
        json!(["https://hub.example.com", "https://ops.example.com"])
    );

    // Compose passes optional variables as empty strings: those are unset.
    for v in [
        "NEXUS_CORS_ORIGINS",
        "PORT",
        "NEXUS_PUBLIC_URL",
        "NEXUS_LOG_LEVEL",
        "DB_MAX_CONNECTIONS",
    ] {
        env.set(v, "");
    }
    let c = load().unwrap();
    assert_eq!(c["server"]["port"], 5150);
    assert_eq!(c["server"]["host"], "http://localhost");
    assert_eq!(c["logger"]["level"], "info");
    assert_eq!(c["database"]["max_connections"], 10);
    assert_eq!(c["server"]["middlewares"]["cors"]["enable"], false);
    env.unset("NEXUS_CORS_ORIGINS");
    env.unset("NEXUS_JWT_SECRET");
}
