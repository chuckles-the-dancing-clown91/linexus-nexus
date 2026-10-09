//! # Operator client certificates
//!
//! With `NEXUS_REQUIRE_OPERATOR_CERT=1`, a request on `/api/v1/*` that
//! authenticates as an **operator** — a bearer that is not an agent's `nxa_`
//! credential nor an `nxe_` enrollment token, i.e. the root system token or a
//! minted one — must arrive over a TLS connection that presented a client
//! certificate verified against `NEXUS_TLS_CLIENT_CA` (see [`crate::tls`]).
//! Otherwise it is `401` before any handler runs. Agents (which carry no
//! certificate), enrollment with a token, and the public routes are not
//! affected.

use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::middleware::system_token;
use crate::models::{agents, enrollment_tokens};
use crate::tls::ClientCert;

/// Environment switch: `1` / `true` / `yes` turns the rule on.
pub const ENV_REQUIRE: &str = "NEXUS_REQUIRE_OPERATOR_CERT";

pub const REFUSAL: &str =
    "operator requests must present a client certificate signed by NEXUS_TLS_CLIENT_CA";

/// Whether a value of [`ENV_REQUIRE`] turns the rule on.
#[must_use]
pub fn required_value(v: Option<&str>) -> bool {
    v.is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Whether the rule is on (read per request, so it can be flipped in tests).
#[must_use]
pub fn required() -> bool {
    required_value(std::env::var(ENV_REQUIRE).ok().as_deref())
}

/// The decision, without I/O: `Some(reason)` when the request must be
/// refused. `bearer` is the request's bearer token, if any.
#[must_use]
pub fn refusal(
    path: &str,
    bearer: Option<&str>,
    has_cert: bool,
    required: bool,
) -> Option<&'static str> {
    if !required || has_cert || !path.starts_with("/api/v1/") {
        return None;
    }
    let token = bearer?;
    if token.starts_with(agents::CREDENTIAL_PREFIX) || token.starts_with(enrollment_tokens::PREFIX)
    {
        return None;
    }
    Some(REFUSAL)
}

/// The middleware applying [`refusal`] to every request.
pub async fn guard(req: Request, next: Next) -> Response {
    let refused = refusal(
        req.uri().path(),
        system_token::bearer_token(req.headers()),
        req.extensions().get::<ClientCert>().is_some(),
        required(),
    );
    if let Some(reason) = refused {
        tracing::warn!(path = %req.uri().path(), "operator request without a client certificate refused");
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({ "error": "unauthorized", "detail": reason })),
        )
            .into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_operators_without_a_certificate_are_refused() {
        let op = Some("nx_operator");
        // Rule off: nothing is refused.
        assert_eq!(refusal("/api/v1/agents", op, false, false), None);
        // Rule on: an operator needs a certificate…
        assert_eq!(refusal("/api/v1/agents", op, false, true), Some(REFUSAL));
        assert_eq!(refusal("/api/v1/agents", op, true, true), None);
        assert_eq!(
            refusal("/api/v1/agents/enroll", Some("root-token"), false, true),
            Some(REFUSAL)
        );
        // …agents, enrollment tokens and anonymous calls do not.
        assert_eq!(
            refusal("/api/v1/agents/x/tasks", Some("nxa_abc"), false, true),
            None
        );
        assert_eq!(
            refusal("/api/v1/agents/enroll", Some("nxe_abc"), false, true),
            None
        );
        assert_eq!(refusal("/api/v1/signing-key", None, false, true), None);
        // Only /api/v1 is covered.
        assert_eq!(refusal("/api/nexus/nodes", op, false, true), None);
        assert_eq!(refusal("/install/agent.sh", op, false, true), None);
    }

    #[tokio::test]
    async fn the_guard_reads_the_verified_certificate_extension() {
        use tower::ServiceExt;
        std::env::set_var(ENV_REQUIRE, "1");
        let app = axum::Router::new()
            .route("/api/v1/agents", axum::routing::get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(guard));
        let req = |cert: bool| {
            let mut r = Request::builder()
                .uri("/api/v1/agents")
                .header("authorization", "Bearer nx_operator")
                .body(axum::body::Body::empty())
                .unwrap();
            if cert {
                r.extensions_mut().insert(ClientCert {
                    fingerprint: "ab".into(),
                });
            }
            r
        };
        let refused = app.clone().oneshot(req(false)).await.unwrap();
        assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
        let allowed = app.oneshot(req(true)).await.unwrap();
        assert_eq!(allowed.status(), StatusCode::OK);
        std::env::remove_var(ENV_REQUIRE);
    }
}
