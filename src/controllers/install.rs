//! # Agent install (`docs/PROVIDERS.md` §8) — public, no bearer
//!
//! * `GET /install/agent.sh` — the POSIX installer (`assets/agent.sh`, kept in
//!   step with `linexus-agent/rmm-agent/deploy/install.sh`). When
//!   `NEXUS_PUBLIC_URL` is set, `NEXUS_URL` defaults to it.
//! * `GET /install/rmm-agent-linux-{amd64|arm64}` — the agent binary, served
//!   from `LINEXUS_AGENT_BINARY_DIR`; `404` when absent.

use axum::extract::Path;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use loco_rs::prelude::{get, Routes};

use super::api::{ApiError, ApiResult};

const SCRIPT: &str = include_str!("assets/agent.sh");
const DEFAULT_URL_MARKER: &str = "# @NEXUS_DEFAULT_URL@";
const ARCHES: [&str; 2] = ["amd64", "arm64"];

/// The installer, with `NEXUS_URL` defaulting to `NEXUS_PUBLIC_URL` (which
/// [`super::cloud::public_url`] has validated to be shell-safe).
#[must_use]
pub fn script() -> String {
    let line = super::cloud::public_url()
        .map(|u| format!("NEXUS_URL=${{NEXUS_URL:-{u}}}"))
        .unwrap_or_default();
    SCRIPT.replacen(DEFAULT_URL_MARKER, &line, 1)
}

async fn serve(file: &str) -> ApiResult<Response> {
    if file == "agent.sh" {
        return Ok((
            [
                (header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            script(),
        )
            .into_response());
    }
    let arch = file
        .strip_prefix("rmm-agent-linux-")
        .filter(|a| ARCHES.contains(a))
        .ok_or_else(|| ApiError::not_found(format!("no such file: {file}")))?;
    let dir = std::env::var("LINEXUS_AGENT_BINARY_DIR")
        .ok()
        .filter(|d| !d.trim().is_empty())
        .ok_or_else(|| {
            ApiError::not_found("agent binaries are not configured (LINEXUS_AGENT_BINARY_DIR)")
        })?;
    let path = std::path::Path::new(dir.trim()).join(format!("rmm-agent-linux-{arch}"));
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| ApiError::not_found(format!("no agent binary for {arch}")))?;
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"rmm-agent-linux-{arch}\""),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// `GET /install/{file}`.
pub async fn file(Path(file): Path<String>) -> ApiResult<Response> {
    serve(&file).await
}

pub fn routes() -> Routes {
    Routes::new().prefix("install").add("/{file}", get(file))
}
