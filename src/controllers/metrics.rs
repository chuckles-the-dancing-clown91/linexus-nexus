//! `GET /metrics` — Prometheus text exposition of [`crate::metrics`].
//! Public, like `/_health`: counters only, nothing identifying.

#![allow(clippy::unused_async)]
use axum::http::header;
use loco_rs::prelude::*;

pub async fn metrics() -> Result<Response> {
    Ok((
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        crate::metrics::render(),
    )
        .into_response())
}

pub fn routes() -> Routes {
    Routes::new().add("/metrics", get(metrics))
}
