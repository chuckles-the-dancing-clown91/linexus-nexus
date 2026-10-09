//! Providers, DNS (Cloudflare and BIND), Registrar, DigitalOcean, the
//! operations log and the install routes (`docs/PROVIDERS.md` §4–9), against
//! the in-process stubs in `stub.rs`.

use linexus_nexus::{
    app::App,
    models::{provider_credentials, tasks},
};
use loco_rs::{testing::prelude::*, TestServer};
use serde_json::{json, Value};
use serial_test::serial;

use super::stub::{bearer, header, Env, Stub, CF_TOKEN, DO_TOKEN};

struct Call<'a> {
    request: &'a TestServer,
}

impl Call<'_> {
    async fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        extra: &[(&'static str, &str)],
    ) -> (u16, Value) {
        let (k, v) = bearer();
        let mut req = match method {
            "GET" => self.request.get(path),
            "POST" => self.request.post(path),
            "PUT" => self.request.put(path),
            "PATCH" => self.request.patch(path),
            "DELETE" => self.request.delete(path),
            _ => unreachable!(),
        }
        .add_header(k, v);
        for (name, value) in extra {
            let (k, v) = header(name, value);
            req = req.add_header(k, v);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.await;
        let status = resp.status_code().as_u16();
        (
            status,
            serde_json::from_str(&resp.text()).unwrap_or(Value::Null),
        )
    }
    async fn get(&self, path: &str) -> (u16, Value) {
        self.send("GET", path, None, &[]).await
    }
    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        self.send("POST", path, Some(body), &[]).await
    }
}

#[tokio::test]
#[serial]
async fn provider_credentials_put_test_delete() {
    let mut env = Env::new();
    let stub = Stub::start().await;
    stub.wire(&mut env);
    request::<App, _, _>(|request, ctx| async move {
        let c = Call { request: &request };
        let (s, list) = c.get("/api/v1/providers").await;
        assert_eq!(s, 200);
        let cf = list
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["key"] == "cloudflare")
            .unwrap()
            .clone();
        assert_eq!(cf["configured"], false);
        assert_eq!(cf["source"], "none");
        assert_eq!(cf["state"], "not_configured");
        let bind = list
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["key"] == "bind")
            .unwrap()
            .clone();
        assert_eq!(bind["source"], "builtin");

        // Not configured: test is 424.
        let (s, b) = c.post("/api/v1/providers/cloudflare/test", json!({})).await;
        assert_eq!(
            (s, b["error"].as_str()),
            (424, Some("provider_not_configured"))
        );

        let (s, b) = c
            .send(
                "PUT",
                "/api/v1/providers/cloudflare/credentials",
                Some(json!({})),
                &[],
            )
            .await;
        assert_eq!((s, b["error"].as_str()), (400, Some("invalid")));
        let (s, _) = c
            .send(
                "PUT",
                "/api/v1/providers/bind/credentials",
                Some(json!({"token": "x"})),
                &[],
            )
            .await;
        assert_eq!(s, 400);
        let (s, _) = c
            .send(
                "PUT",
                "/api/v1/providers/aws/credentials",
                Some(json!({"token": "x"})),
                &[],
            )
            .await;
        assert_eq!(s, 404);

        let (s, _) = c
            .send(
                "PUT",
                "/api/v1/providers/cloudflare/credentials",
                Some(json!({"token": CF_TOKEN})),
                &[("x-requested-by", "user:1 ops@example.com")],
            )
            .await;
        assert_eq!(s, 204);
        // Sealed at rest.
        let row = provider_credentials::Model::find_by_provider(&ctx.db, "cloudflare")
            .await
            .unwrap()
            .unwrap();
        let sealed = row.sealed_token.unwrap();
        assert!(!sealed
            .windows(CF_TOKEN.len())
            .any(|w| w == CF_TOKEN.as_bytes()));

        let (_, list) = c.get("/api/v1/providers").await;
        let cf = list
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["key"] == "cloudflare")
            .unwrap()
            .clone();
        assert_eq!(cf["configured"], true);
        assert_eq!(cf["source"], "stored");
        assert_eq!(cf["state"], "unknown");

        let (s, t) = c.post("/api/v1/providers/cloudflare/test", json!({})).await;
        assert_eq!(s, 200, "{t}");
        assert_eq!(t["ok"], true);
        assert_eq!(t["state"], "ok");
        assert_eq!(t["accountId"], "acc1");
        assert_eq!(t["accountName"], "Acme Hosting");
        assert_eq!(t["scopes"]["tokenStatus"], "active");
        let (_, list) = c.get("/api/v1/providers").await;
        let cf = list
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["key"] == "cloudflare")
            .unwrap()
            .clone();
        assert_eq!(cf["state"], "ok");
        assert_eq!(cf["accountId"], "acc1");
        assert!(cf["checkedAt"].is_string());

        // A wrong DigitalOcean token: the test reports it in-band.
        let (s, _) = c
            .send(
                "PUT",
                "/api/v1/providers/digitalocean/credentials",
                Some(json!({"token": "dop_v1_wrong"})),
                &[],
            )
            .await;
        assert_eq!(s, 204);
        let (s, t) = c
            .post("/api/v1/providers/digitalocean/test", json!({}))
            .await;
        assert_eq!(s, 200);
        assert_eq!(t["ok"], false);
        assert_eq!(t["state"], "unauthorized");
        assert!(!t["detail"].as_str().unwrap().contains("dop_v1_wrong"));

        // Environment credentials win.
        env.set("DIGITALOCEAN_TOKEN", DO_TOKEN);
        let (_, t) = c
            .post("/api/v1/providers/digitalocean/test", json!({}))
            .await;
        assert_eq!(t["ok"], true);
        assert_eq!(t["accountName"], "Ops Team");
        let (_, list) = c.get("/api/v1/providers").await;
        let d = list
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["key"] == "digitalocean")
            .unwrap()
            .clone();
        assert_eq!(d["source"], "env");

        let (k, v) = bearer();
        let resp = request
            .delete("/api/v1/providers/cloudflare/credentials")
            .add_header(k, v)
            .await;
        assert_eq!(resp.status_code(), 204);
        let (_, list) = c.get("/api/v1/providers").await;
        let cf = list
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["key"] == "cloudflare")
            .unwrap()
            .clone();
        assert_eq!(cf["configured"], false);

        // Every mutation is in the operations log, with its requester.
        let (_, ops) = c.get("/api/v1/operations?provider=cloudflare").await;
        let ops = ops.as_array().unwrap();
        assert_eq!(ops[0]["operation"], "credentials.delete");
        assert_eq!(ops[1]["operation"], "credentials.put");
        assert_eq!(ops[1]["requester"], "user:1 ops@example.com");
        assert_eq!(ops[1]["status"], "ok");
        assert!(ops.iter().all(|o| !o.to_string().contains(CF_TOKEN)));
    })
    .await;
}

#[tokio::test]
#[serial]
async fn credentials_need_a_sealing_key_outside_development() {
    let mut env = Env::new();
    env.unset("NEXUS_SECRET_KEY");
    request::<App, _, _>(|request, _ctx| async move {
        let c = Call { request: &request };
        let (s, b) = c
            .send(
                "PUT",
                "/api/v1/providers/digitalocean/credentials",
                Some(json!({"token": "abc"})),
                &[],
            )
            .await;
        assert_eq!(s, 400);
        assert!(b["detail"].as_str().unwrap().contains("NEXUS_SECRET_KEY"));
    })
    .await;
}

#[tokio::test]
#[serial]
async fn cloudflare_dns_zones_and_records() {
    let mut env = Env::new();
    let stub = Stub::start().await;
    stub.wire(&mut env);
    env.set("CLOUDFLARE_API_TOKEN", CF_TOKEN);
    request::<App, _, _>(|request, _ctx| async move {
        let c = Call { request: &request };
        let (s, zones) = c.get("/api/v1/dns/zones").await;
        assert_eq!((s, zones), (200, json!([])));

        let (s, z) = c
            .post(
                "/api/v1/dns/zones",
                json!({"provider": "cloudflare", "name": "Example.org."}),
            )
            .await;
        assert_eq!(s, 201, "{z}");
        assert_eq!(z["name"], "example.org");
        assert_eq!(z["provider"], "cloudflare");
        assert_eq!(
            z["nameServers"],
            json!(["ada.ns.cloudflare.com", "bob.ns.cloudflare.com"])
        );
        let zone_id = z["id"].as_str().unwrap().to_string();
        assert!(zone_id.starts_with("cf:"));
        // Cloudflare's zone list carries no record count.
        assert_eq!(z["recordCount"], Value::Null);
        let (_, zones) = c.get("/api/v1/dns/zones").await;
        assert_eq!(zones[0]["recordCount"], Value::Null);
        assert!(zones[0].as_object().unwrap().contains_key("recordCount"));
        // The zone was created in the token's account.
        assert_eq!(
            stub.seen("POST", "/cf/zones")[0].body["account"]["id"],
            "acc1"
        );

        let (s, b) = c
            .post(
                "/api/v1/dns/zones",
                json!({"provider": "cloudflare", "name": "example.org"}),
            )
            .await;
        assert_eq!((s, b["error"].as_str()), (409, Some("conflict")));
        let (s, _) = c
            .post(
                "/api/v1/dns/zones",
                json!({"provider": "route53", "name": "a.org"}),
            )
            .await;
        assert_eq!(s, 400);

        // Records: relative names become FQDNs; validation answers 400.
        let rec_path = format!("/api/v1/dns/zones/{zone_id}/records");
        let rec = |b: Value| c.post(&rec_path, b);
        let (s, r) =
            rec(json!({"type": "A", "name": "www", "content": "192.0.2.10", "proxied": true}))
                .await;
        assert_eq!(s, 201, "{r}");
        assert_eq!(r["record"]["name"], "www.example.org");
        assert_eq!(r["record"]["ttl"], 1);
        assert_eq!(r["record"]["proxied"], true);
        assert!(r.get("taskId").is_none());
        let rid = r["record"]["id"].as_str().unwrap().to_string();
        for bad in [
            json!({"type": "A", "name": "x", "content": "not-an-ip"}),
            json!({"type": "AAAA", "name": "x", "content": "1.2.3.4"}),
            json!({"type": "MX", "name": "@", "content": "mail.example.org"}),
            json!({"type": "CNAME", "name": "@", "content": "x.example.net"}),
            json!({"type": "A", "name": "x", "content": "1.2.3.4", "ttl": 30}),
            json!({"type": "SRV", "name": "x", "content": "y"}),
            json!({"name": "x", "content": "y"}),
        ] {
            let (s, b) = rec(bad.clone()).await;
            assert_eq!((s, b["error"].as_str()), (400, Some("invalid")), "{bad}");
        }
        let (s, r) =
            rec(json!({"type": "MX", "name": "@", "content": "mail.example.org", "priority": 10}))
                .await;
        assert_eq!(s, 201);
        assert_eq!(r["record"]["priority"], 10);

        let (_, list) = c
            .get(&format!(
                "/api/v1/dns/zones/{zone_id}/records?type=A&name=www"
            ))
            .await;
        assert_eq!(list.as_array().unwrap().len(), 1);
        let (_, detail) = c.get(&format!("/api/v1/dns/zones/{zone_id}")).await;
        assert_eq!(detail["records"].as_array().unwrap().len(), 2);

        let (s, r) = c
            .send(
                "PATCH",
                &format!("/api/v1/dns/zones/{zone_id}/records/{rid}"),
                Some(json!({"content": "192.0.2.11"})),
                &[],
            )
            .await;
        assert_eq!(s, 200);
        assert_eq!(r["record"]["content"], "192.0.2.11");
        assert_eq!(r["record"]["name"], "www.example.org");

        // ensure: unchanged, then changed, then created.
        let ensure_path = format!("/api/v1/dns/zones/{zone_id}/records/ensure");
        let ensure = |b: Value| c.post(&ensure_path, b);
        let (s, e) =
            ensure(json!({"type": "A", "name": "www.example.org", "content": "192.0.2.11"})).await;
        assert_eq!(s, 200);
        assert_eq!(e["changed"], false);
        assert_eq!(e["record"]["id"], rid.as_str());
        let (_, e) = ensure(json!({"type": "A", "name": "www", "content": "192.0.2.12"})).await;
        assert_eq!(e["changed"], true);
        assert_eq!(e["record"]["id"], rid.as_str());
        assert_eq!(e["record"]["content"], "192.0.2.12");
        let (_, e) =
            ensure(json!({"type": "TXT", "name": "_acme", "content": "token-value"})).await;
        assert_eq!(e["changed"], true);
        assert_eq!(e["record"]["name"], "_acme.example.org");

        let (s, d) = c
            .send(
                "DELETE",
                &format!("/api/v1/dns/zones/{zone_id}/records/{rid}"),
                None,
                &[],
            )
            .await;
        assert_eq!((s, d), (200, json!({})));

        // Deleting a zone needs X-Confirm with its name.
        let (s, b) = c
            .send("DELETE", &format!("/api/v1/dns/zones/{zone_id}"), None, &[])
            .await;
        assert_eq!(
            (s, b["error"].as_str()),
            (412, Some("confirmation_required"))
        );
        let (s, _) = c
            .send(
                "DELETE",
                &format!("/api/v1/dns/zones/{zone_id}"),
                None,
                &[("x-confirm", "example.com")],
            )
            .await;
        assert_eq!(s, 412);
        let (s, _) = c
            .send(
                "DELETE",
                &format!("/api/v1/dns/zones/{zone_id}"),
                None,
                &[("x-confirm", "example.org")],
            )
            .await;
        assert_eq!(s, 204);
        let (s, _) = c.get(&format!("/api/v1/dns/zones/{zone_id}")).await;
        assert_eq!(s, 404);
        let (s, _) = c.get("/api/v1/dns/zones/route53:abc").await;
        assert_eq!(s, 404);

        // A provider error never carries the token.
        let (s, b) = c
            .post(
                "/api/v1/dns/zones",
                json!({"provider": "cloudflare", "name": "leak.example.com"}),
            )
            .await;
        assert_eq!((s, b["error"].as_str()), (422, Some("provider_rejected")));
        let detail = b["detail"].as_str().unwrap();
        assert!(detail.contains("rejected request carrying"), "{detail}");
        assert!(!detail.contains(CF_TOKEN), "{detail}");
        let (_, ops) = c.get("/api/v1/operations").await;
        assert!(!ops.to_string().contains(CF_TOKEN));

        // A wrong token is 422 (the provider refused), not 401.
        env.set("CLOUDFLARE_API_TOKEN", "wrong-token-123456");
        let (s, b) = c.get("/api/v1/dns/zones").await;
        assert_eq!((s, b["error"].as_str()), (422, Some("provider_rejected")));
        // An unreachable provider is 502.
        env.set("CLOUDFLARE_API_BASE", "http://127.0.0.1:9");
        let (s, b) = c.get("/api/v1/dns/zones").await;
        assert_eq!(
            (s, b["error"].as_str()),
            (502, Some("provider_unreachable"))
        );
    })
    .await;
}

#[tokio::test]
#[serial]
async fn bind_zone_changes_dispatch_rendered_zone_files() {
    let mut env = Env::new();
    let stub = Stub::start().await;
    stub.wire(&mut env);
    request::<App, _, _>(|request, ctx| async move {
        let c = Call { request: &request };
        let (_, p) = c.post("/api/v1/agents/enroll", json!({"hostname": "dns-a"})).await;
        let primary = p["id"].as_str().unwrap().to_string();
        let (_, sec) = c.post("/api/v1/agents/enroll", json!({"hostname": "ns2.example.net"})).await;
        let secondary = sec["id"].as_str().unwrap().to_string();
        for (id, ip) in [(&primary, "203.0.113.7"), (&secondary, "198.51.100.9")] {
            let (s, _) = c.post(&format!("/api/v1/agents/{id}/report"), json!({"publicIp": ip})).await;
            assert_eq!(s, 200);
        }

        let (s, b) = c.post("/api/v1/dns/zones", json!({"provider": "bind", "name": "example.org"})).await;
        assert_eq!(s, 400, "primaryAgentId is required: {b}");
        let (s, _) = c.post("/api/v1/dns/zones", json!({"provider": "bind", "name": "example.org", "primaryAgentId": uuid::Uuid::new_v4()})).await;
        assert_eq!(s, 404);

        let (s, z) = c
            .post("/api/v1/dns/zones", json!({
                "provider": "bind", "name": "example.org", "primaryAgentId": primary,
                "secondaryAgentIds": [secondary], "adminEmail": "first.last@example.org",
            }))
            .await;
        assert_eq!(s, 201, "{z}");
        let zone_id = z["id"].as_str().unwrap().to_string();
        assert!(zone_id.starts_with("bind:"));
        let today: i64 = chrono::Utc::now().format("%Y%m%d").to_string().parse().unwrap();
        assert_eq!(z["serial"], today * 100 + 1);
        assert_eq!(z["applyStatus"], "pending");
        assert_eq!(z["nameServers"], json!(["ns1.example.org", "ns2.example.net"]));
        assert_eq!(z["primaryAgentId"], primary.as_str());
        assert_eq!(z["secondaryAgentIds"], json!([secondary]));
        let first_task = z["lastTaskId"].as_str().unwrap().to_string();
        let (s, _) = c.post("/api/v1/dns/zones", json!({"provider": "bind", "name": "example.org", "primaryAgentId": primary})).await;
        assert_eq!(s, 409);

        let (s, r) = c
            .post(&format!("/api/v1/dns/zones/{zone_id}/records"), json!({"type": "A", "name": "www", "content": "192.0.2.80", "ttl": 300}))
            .await;
        assert_eq!(s, 201, "{r}");
        let task_id = r["taskId"].as_str().unwrap().to_string();
        assert_ne!(task_id, first_task);
        assert_eq!(r["record"]["name"], "www.example.org");
        assert_eq!(r["record"]["ttl"], 300);

        let (_, z) = c.get(&format!("/api/v1/dns/zones/{zone_id}")).await;
        assert_eq!(z["serial"], today * 100 + 2);
        assert_eq!(z["lastTaskId"], task_id.as_str());
        assert_eq!(z["records"].as_array().unwrap().len(), 1);
        assert_eq!(z["recordCount"], 1);
        // The zone list counts BIND records without listing them.
        let (_, zones) = c.get("/api/v1/dns/zones").await;
        let listed = zones.as_array().unwrap().iter().find(|z| z["id"] == zone_id.as_str()).unwrap();
        assert_eq!(listed["recordCount"], 1);
        assert!(listed.get("records").is_none());

        // The primary's task carries the rendered zone file.
        let task = tasks::Model::find_by_task_id(&ctx.db, &task_id.parse().unwrap()).await.unwrap();
        assert_eq!(task.intent, "dns_zone_apply");
        assert_eq!(task.status, "planned");
        assert_eq!(task.targets(), vec![primary.clone()]);
        let plan: Value = serde_json::from_str(task.plan.as_deref().unwrap()).unwrap();
        let params = &plan["steps"][0]["params"];
        assert_eq!(params["zone"], "example.org");
        assert_eq!(params["role"], "primary");
        assert_eq!(params["secondaries"], "198.51.100.9");
        assert_eq!(params["serial"], (today * 100 + 2).to_string());
        let content = params["content"].as_str().unwrap();
        assert!(content.contains("$ORIGIN example.org."), "{content}");
        assert!(content.contains(&format!("{}\t; serial", today * 100 + 2)));
        assert!(content.contains("SOA\tns1.example.org. first\\.last.example.org. ("));
        assert!(content.contains("@\t3600\tIN\tNS\tns2.example.net.\n"));
        assert!(content.contains("ns1.example.org.\t3600\tIN\tA\t203.0.113.7\n"));
        assert!(content.contains("www.example.org.\t300\tIN\tA\t192.0.2.80\n"));
        // …and the secondary's names the primary.
        let plans = stub.seen("POST", "/orch/plan");
        let sec_plan = plans
            .iter()
            .rev()
            .find(|p| p.body["params"]["role"] == "secondary")
            .unwrap();
        assert_eq!(sec_plan.body["targets"], json!([secondary]));
        assert_eq!(sec_plan.body["params"]["primaries"], "203.0.113.7");
        // The first change's applies, never picked up, were superseded.
        let first = tasks::Model::find_by_task_id(&ctx.db, &first_task.parse().unwrap()).await.unwrap();
        assert_eq!(first.status, "cancelled");

        // A CNAME cannot share a name; TXT is quoted safely.
        let (s, _) = c
            .post(&format!("/api/v1/dns/zones/{zone_id}/records"), json!({"type": "CNAME", "name": "www", "content": "x.example.net"}))
            .await;
        assert_eq!(s, 409);
        let (s, b) = c
            .post(&format!("/api/v1/dns/zones/{zone_id}/records"), json!({"type": "A", "name": "x", "content": "1.2.3.4", "ttl": 1}))
            .await;
        assert_eq!((s, b["error"].as_str()), (400, Some("invalid")));

        // Both servers report success → applied.
        let open: Vec<tasks::Model> = tasks::Model::find_all(&ctx.db)
            .await
            .unwrap()
            .into_iter()
            .filter(|t| t.intent == "dns_zone_apply" && t.status == "planned")
            .collect();
        assert_eq!(open.len(), 2);
        for t in &open {
            let agent = &t.targets()[0];
            let (s, _) = c.post(&format!("/api/v1/agents/{agent}/tasks/{}/result", t.task_id), json!({"status": "success"})).await;
            assert_eq!(s, 200);
        }
        let (_, z) = c.get(&format!("/api/v1/dns/zones/{zone_id}")).await;
        assert_eq!(z["applyStatus"], "applied");

        let (s, list) = c.get("/api/v1/dns/zones").await;
        assert_eq!(s, 200);
        assert_eq!(list.as_array().unwrap().len(), 1, "no Cloudflare credentials: bind only");

        // Install a DNS server.
        let (s, inst) = c.post("/api/v1/dns/servers", json!({"agentId": secondary})).await;
        assert_eq!(s, 200);
        assert_eq!(inst["agentId"], secondary.as_str());
        let (_, servers) = c.get("/api/v1/dns/servers").await;
        let me = servers.as_array().unwrap().iter().find(|s| s["agentId"] == secondary.as_str()).unwrap();
        assert_eq!(me["installTaskId"], inst["taskId"]);

        // Delete: confirmation, then dns_zone_remove to both servers.
        let (s, _) = c.send("DELETE", &format!("/api/v1/dns/zones/{zone_id}"), None, &[]).await;
        assert_eq!(s, 412);
        let (s, _) = c.send("DELETE", &format!("/api/v1/dns/zones/{zone_id}"), None, &[("x-confirm", "example.org")]).await;
        assert_eq!(s, 204);
        let removes = stub
            .seen("POST", "/orch/plan")
            .into_iter()
            .filter(|p| p.body["intent"] == "dns_zone_remove")
            .count();
        assert_eq!(removes, 2);
        let (s, _) = c.get(&format!("/api/v1/dns/zones/{zone_id}")).await;
        assert_eq!(s, 404);
    })
    .await;
}

#[tokio::test]
#[serial]
async fn registrar_domains_list_and_patch() {
    let mut env = Env::new();
    let stub = Stub::start().await;
    stub.wire(&mut env);
    env.set("CLOUDFLARE_API_TOKEN", CF_TOKEN);
    request::<App, _, _>(|request, _ctx| async move {
        let c = Call { request: &request };
        let (_, z) = c
            .post(
                "/api/v1/dns/zones",
                json!({"provider": "cloudflare", "name": "example.com"}),
            )
            .await;
        let (s, list) = c.get("/api/v1/domains").await;
        assert_eq!(s, 200, "{list}");
        let d = &list[0];
        assert_eq!(d["name"], "example.com");
        assert_eq!(d["registrar"], "cloudflare");
        assert_eq!(d["autoRenew"], true);
        assert_eq!(d["locked"], true);
        assert_eq!(d["expiresAt"], "2027-10-09T00:00:00Z");
        assert_eq!(d["zoneId"], z["id"]);

        let (s, d) = c
            .send(
                "PATCH",
                "/api/v1/domains/example.com",
                Some(json!({"autoRenew": false, "privacy": false})),
                &[],
            )
            .await;
        assert_eq!(s, 200, "{d}");
        assert_eq!(d["autoRenew"], false);
        assert_eq!(d["privacy"], false);
        assert_eq!(d["locked"], true);
        let put = stub.seen("PUT", "/cf/accounts/acc1/registrar/domains/example.com");
        assert_eq!(put[0].body, json!({"auto_renew": false, "privacy": false}));

        let (s, _) = c
            .send("PATCH", "/api/v1/domains/example.com", Some(json!({})), &[])
            .await;
        assert_eq!(s, 400);
        let (s, _) = c.get("/api/v1/domains/nope.example").await;
        assert_eq!(s, 404);
    })
    .await;
}

#[tokio::test]
#[serial]
async fn droplets_with_enrollment_actions_and_deletion() {
    let mut env = Env::new();
    let stub = Stub::start().await;
    stub.wire(&mut env);
    env.set("DIGITALOCEAN_TOKEN", DO_TOKEN);
    request::<App, _, _>(|request, _ctx| async move {
        let c = Call { request: &request };
        let body = json!({
            "name": "web-1", "region": "fra1", "size": "s-1vcpu-1gb", "image": "ubuntu-24-04-x64",
            "tags": ["web"], "userData": "#cloud-config\npackages: [htop]",
            "enrollAgent": {"hostgroup": "acme", "environment": "production", "metadata": {"clientId": "c-1"}},
        });
        // Without NEXUS_PUBLIC_URL the droplet could never call home.
        let (s, b) = c.post("/api/v1/cloud/droplets", body.clone()).await;
        assert_eq!((s, b["error"].as_str()), (400, Some("invalid")));
        assert!(stub.seen("POST", "/v2/droplets").is_empty());

        env.set("NEXUS_PUBLIC_URL", "https://nexus.example.com/");
        let key = [("idempotency-key", "create-web-1")];
        let (s, created) = c.send("POST", "/api/v1/cloud/droplets", Some(body.clone()), &key).await;
        assert_eq!(s, 201, "{created}");
        assert_eq!(created["droplet"]["name"], "web-1");
        assert_eq!(created["droplet"]["region"], "fra1");
        let token_id = created["enrollmentTokenId"].as_str().unwrap().to_string();

        let sent = stub.seen("POST", "/v2/droplets");
        assert_eq!(sent.len(), 1);
        let user_data = sent[0].body["user_data"].as_str().unwrap();
        assert!(user_data.contains(
            "curl -fsSL https://nexus.example.com/install/agent.sh | NEXUS_URL=https://nexus.example.com ENROLLMENT_TOKEN=nxe_"
        ), "{user_data}");
        assert!(user_data.contains("packages: [htop]"));
        assert_eq!(sent[0].body["tags"], json!(["web", "lx-hostgroup-acme"]));
        assert_eq!(sent[0].body["monitoring"], true);
        let plaintext: String = user_data
            .split("ENROLLMENT_TOKEN=")
            .nth(1)
            .unwrap()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        assert!(plaintext.starts_with("nxe_") && plaintext.len() > 40);

        // The minted token is real, one use, bound to the hostgroup.
        let (_, t) = c.get(&format!("/api/v1/enrollment-tokens/{token_id}")).await;
        assert_eq!(t["hostgroup"], "acme");
        assert_eq!(t["maxUses"], 1);
        assert_eq!(t["metadata"], json!({"clientId": "c-1"}));
        let resp = request
            .post("/api/v1/agents/enroll")
            .json(&json!({"hostname": "web-1", "enrollmentToken": plaintext}))
            .await;
        assert_eq!(resp.status_code(), 201);
        assert_eq!(resp.json::<Value>()["hostgroup"], "acme");

        // Replay with the same key: same answer, nothing created twice.
        let resp = {
            let (k, v) = bearer();
            let (hk, hv) = header("idempotency-key", "create-web-1");
            request.post("/api/v1/cloud/droplets").add_header(k, v).add_header(hk, hv).json(&body).await
        };
        assert_eq!(resp.status_code(), 201);
        assert_eq!(resp.headers().get("idempotent-replayed").unwrap(), "true");
        assert_eq!(resp.json::<Value>(), created);
        assert_eq!(stub.seen("POST", "/v2/droplets").len(), 1);
        // The same key for another operation is a conflict.
        let (s, _) = c
            .send("POST", "/api/v1/cloud/volumes", Some(json!({"name": "v", "region": "fra1", "sizeGigabytes": 10})), &key)
            .await;
        assert_eq!(s, 409);

        let id = created["droplet"]["id"].as_u64().unwrap();
        let (s, list) = c.get("/api/v1/cloud/droplets").await;
        assert_eq!(s, 200);
        assert_eq!(list[0]["id"], id);
        let (s, d) = c.get(&format!("/api/v1/cloud/droplets/{id}")).await;
        assert_eq!((s, d["name"].as_str()), (200, Some("web-1")));
        let (s, _) = c.get("/api/v1/cloud/droplets/999999").await;
        assert_eq!(s, 404);

        // Actions.
        let (s, a) = c.post(&format!("/api/v1/cloud/droplets/{id}/actions"), json!({"type": "reboot"})).await;
        assert_eq!(s, 200, "{a}");
        assert_eq!(a["action"]["type"], "reboot");
        assert_eq!(a["action"]["status"], "in-progress");
        assert_eq!(a["action"]["resourceId"], id);
        let (s, _) = c.post(&format!("/api/v1/cloud/droplets/{id}/actions"), json!({"type": "resize"})).await;
        assert_eq!(s, 400);
        let (s, _) = c.post(&format!("/api/v1/cloud/droplets/{id}/actions"), json!({"type": "destroy_everything"})).await;
        assert_eq!(s, 400);
        let (s, _) = c.post(&format!("/api/v1/cloud/droplets/{id}/actions"), json!({"type": "resize", "size": "s-2vcpu-2gb"})).await;
        assert_eq!(s, 200);
        let resize = stub.seen("POST", &format!("/v2/droplets/{id}/actions"));
        assert_eq!(resize.last().unwrap().body, json!({"type": "resize", "size": "s-2vcpu-2gb", "disk": false}));

        // Deletion needs the droplet's name.
        let (s, b) = c.send("DELETE", &format!("/api/v1/cloud/droplets/{id}"), None, &[]).await;
        assert_eq!((s, b["error"].as_str()), (412, Some("confirmation_required")));
        let (s, _) = c.send("DELETE", &format!("/api/v1/cloud/droplets/{id}"), None, &[("x-confirm", "web-1")]).await;
        assert_eq!(s, 204);

        let (s, acct) = c.get("/api/v1/cloud/account").await;
        assert_eq!(s, 200);
        assert_eq!(acct["teamName"], "Ops Team");
        assert_eq!(acct["balance"]["monthToDateUsage"], "12.34");

        let (_, ops) = c.get("/api/v1/operations?provider=digitalocean&limit=50").await;
        let ops = ops.as_array().unwrap();
        assert!(ops.iter().any(|o| o["operation"] == "droplet.create" && o["idempotencyKey"] == "create-web-1"));
        assert_eq!(ops.iter().filter(|o| o["operation"] == "droplet.create" && o["status"] == "ok").count(), 1);
        assert!(ops.iter().any(|o| o["operation"] == "droplet.delete"));
        assert!(!serde_json::to_string(ops).unwrap().contains(DO_TOKEN));
    })
    .await;
}

#[tokio::test]
#[serial]
async fn volumes_mounts_and_load_balancers() {
    let mut env = Env::new();
    let stub = Stub::start().await;
    stub.wire(&mut env);
    env.set("DIGITALOCEAN_TOKEN", DO_TOKEN);
    request::<App, _, _>(|request, ctx| async move {
        let c = Call { request: &request };
        let (s, v) = c
            .post("/api/v1/cloud/volumes", json!({"name": "pgdata", "region": "fra1", "sizeGigabytes": 100, "filesystemType": "xfs"}))
            .await;
        assert_eq!(s, 201, "{v}");
        assert_eq!(v["name"], "pgdata");
        assert_eq!(v["sizeGigabytes"], 100);
        assert_eq!(v["filesystemType"], "xfs");
        let vid = v["id"].as_str().unwrap().to_string();
        let (s, _) = c.post("/api/v1/cloud/volumes", json!({"name": "x", "region": "fra1", "sizeGigabytes": 1, "filesystemType": "ntfs"})).await;
        assert_eq!(s, 400);

        let (_, a) = c.post("/api/v1/agents/enroll", json!({"hostname": "db-1"})).await;
        let agent = a["id"].as_str().unwrap().to_string();
        let (s, _) = c.post(&format!("/api/v1/cloud/volumes/{vid}/mount"), json!({"agentId": agent, "mountPoint": "/etc"})).await;
        assert_eq!(s, 400);
        let (s, m) = c
            .post(&format!("/api/v1/cloud/volumes/{vid}/mount"), json!({"agentId": agent, "mountPoint": "/var/lib/postgresql"}))
            .await;
        assert_eq!(s, 200, "{m}");
        let task = tasks::Model::find_by_task_id(&ctx.db, &m["taskId"].as_str().unwrap().parse().unwrap()).await.unwrap();
        assert_eq!(task.intent, "mount_volume");
        assert_eq!(task.targets(), vec![agent.clone()]);
        let plan: Value = serde_json::from_str(task.plan.as_deref().unwrap()).unwrap();
        assert_eq!(
            plan["steps"][0]["params"],
            json!({"device": "/dev/disk/by-id/scsi-0DO_Volume_pgdata", "mountPoint": "/var/lib/postgresql", "fsType": "xfs", "format": "if_blank"})
        );

        let (s, _) = c.send("DELETE", &format!("/api/v1/cloud/volumes/{vid}"), None, &[("x-confirm", "wrong")]).await;
        assert_eq!(s, 412);

        let (s, lb) = c
            .post("/api/v1/cloud/load-balancers", json!({
                "name": "web-lb", "region": "fra1",
                "forwardingRules": [{"entryProtocol": "https", "entryPort": 443, "targetProtocol": "http", "targetPort": 80, "certificateId": "6b1a4c9e-0000-4000-8000-000000000001"}],
                "healthCheck": {"protocol": "http", "port": 80, "path": "/healthz", "checkIntervalSeconds": 10},
                "tag": "web", "redirectHttpToHttps": true,
            }))
            .await;
        assert_eq!(s, 201, "{lb}");
        assert_eq!(lb["name"], "web-lb");
        assert_eq!(lb["ip"], "203.0.113.50");
        assert_eq!(lb["forwardingRules"][0]["entryPort"], 443);
        assert_eq!(lb["healthCheck"]["path"], "/healthz");
        assert_eq!(lb["redirectHttpToHttps"], true);
        let sent = &stub.seen("POST", "/v2/load_balancers")[0].body;
        assert_eq!(sent["forwarding_rules"][0]["entry_protocol"], "https");
        assert_eq!(sent["health_check"]["check_interval_seconds"], 10);
        assert_eq!(sent["tag"], "web");
        let (s, _) = c
            .post("/api/v1/cloud/load-balancers", json!({"name": "x", "region": "fra1", "forwardingRules": [], "tag": "web"}))
            .await;
        assert_eq!(s, 400);
        let (s, _) = c
            .post("/api/v1/cloud/load-balancers", json!({
                "name": "x", "region": "fra1", "tag": "web", "dropletIds": [1],
                "forwardingRules": [{"entryProtocol": "http", "entryPort": 80, "targetProtocol": "http", "targetPort": 80}],
            }))
            .await;
        assert_eq!(s, 400, "dropletIds and tag are exclusive");

        // Not configured → 424. (The token was also stored from the
        // environment at boot, so forget that copy too.)
        env.unset("DIGITALOCEAN_TOKEN");
        let (s, _) = c.send("DELETE", "/api/v1/providers/digitalocean/credentials", None, &[]).await;
        assert_eq!(s, 204);
        let (s, b) = c.get("/api/v1/cloud/volumes").await;
        assert_eq!((s, b["error"].as_str()), (424, Some("provider_not_configured")));
    })
    .await;
}

#[tokio::test]
#[serial]
async fn install_script_and_binaries_are_public() {
    let mut env = Env::new();
    env.set("NEXUS_PUBLIC_URL", "https://nexus.example.com");
    request::<App, _, _>(|request, _ctx| async move {
        let resp = request.get("/install/agent.sh").await;
        assert_eq!(resp.status_code(), 200);
        let script = resp.text();
        assert!(script.starts_with("#!/bin/sh"));
        assert!(script.contains("NEXUS_URL=${NEXUS_URL:-https://nexus.example.com}"));
        // The plan signing key is pinned by default; a CA and another key
        // can be passed in, and both land in the agent's env file.
        let key: Value = request.get("/api/v1/signing-key").await.json();
        let pubkey = key["publicKey"].as_str().unwrap();
        assert!(script.contains(&format!(
            "NEXUS_SIGNING_PUBKEY=${{NEXUS_SIGNING_PUBKEY:-${{LINEXUS_SIGNING_PUBKEY:-{pubkey}}}}}"
        )));
        let syntax = std::process::Command::new("sh")
            .args(["-n", "-c", &script])
            .status()
            .unwrap();
        assert!(syntax.success(), "the installer parses");
        for needle in [
            "LINEXUS_SIGNING_PUBKEY=$SIGNING_PUBKEY",
            "LINEXUS_CA_FILE=$CA_FILE",
            "CA_FILE=/etc/linexus/nexus-ca.pem",
            "NEXUS_CA_PEM",
            "NEXUS_CA_FILE",
            "--cacert",
            "/usr/local/bin/linexus-agent",
            "/etc/linexus/agent.env",
            "AGENT_STATE_FILE=$STATE_FILE",
            "/var/lib/linexus/agent-state.json",
            "/etc/systemd/system/linexus-agent.service",
            "Restart=always",
            "EnvironmentFile=/etc/linexus/agent.env",
            "x86_64 | amd64) ARCH=amd64",
            "aarch64 | arm64) ARCH=arm64",
            "systemctl daemon-reload",
            "systemctl enable linexus-agent.service",
        ] {
            assert!(script.contains(needle), "{needle}");
        }

        assert_eq!(
            request
                .get("/install/rmm-agent-linux-amd64")
                .await
                .status_code(),
            404
        );
        let dir = std::env::temp_dir().join(format!("lx-bin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("rmm-agent-linux-arm64"), b"\x7fELFbinary").unwrap();
        std::env::set_var("LINEXUS_AGENT_BINARY_DIR", &dir);
        let resp = request.get("/install/rmm-agent-linux-arm64").await;
        assert_eq!(resp.status_code(), 200);
        assert_eq!(resp.as_bytes().as_ref(), b"\x7fELFbinary");
        assert_eq!(
            request
                .get("/install/rmm-agent-linux-amd64")
                .await
                .status_code(),
            404
        );
        assert_eq!(
            request
                .get("/install/rmm-agent-linux-..%2F..%2Fetc%2Fpasswd")
                .await
                .status_code(),
            404
        );
        assert_eq!(request.get("/install/other").await.status_code(), 404);
        std::env::remove_var("LINEXUS_AGENT_BINARY_DIR");
        let _ = std::fs::remove_dir_all(dir);
    })
    .await;
}
