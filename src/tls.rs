//! Native HTTPS and optional client certificates (mTLS).
//!
//! * `NEXUS_TLS_CERT` + `NEXUS_TLS_KEY` (PEM paths): serve HTTPS with rustls
//!   instead of plain HTTP. Unset: plain HTTP, as behind a TLS-terminating
//!   proxy.
//! * `NEXUS_TLS_CLIENT_CA` (PEM path): ask every client for a certificate
//!   and verify any that is presented against that CA. Presenting one is
//!   optional at the TLS layer — agents authenticate with their `nxa_`
//!   credentials and carry no certificate — and a verified one reaches the
//!   handlers as a [`ClientCert`] request extension. Whether an operator
//!   *must* present one is [`crate::middleware::client_cert`]'s decision.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::{extract::ConnectInfo, Router};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::{conn::auto::Builder, graceful::GracefulShutdown},
    service::TowerToHyperService,
};
use rustls::{server::WebPkiClientVerifier, RootCertStore, ServerConfig};
use rustls_pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt;

pub const ENV_CERT: &str = "NEXUS_TLS_CERT";
pub const ENV_KEY: &str = "NEXUS_TLS_KEY";
pub const ENV_CLIENT_CA: &str = "NEXUS_TLS_CLIENT_CA";
/// How long a client gets to finish the TLS handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// A client certificate the TLS layer verified against
/// `NEXUS_TLS_CLIENT_CA`. Its presence on a request means it was verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientCert {
    /// SHA-256 of the leaf certificate (DER), hex.
    pub fingerprint: String,
}

/// The TLS files named by the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub cert: PathBuf,
    pub key: PathBuf,
    pub client_ca: Option<PathBuf>,
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `Ok(None)` for plain HTTP; an error when the variables are inconsistent.
pub fn settings_from_env() -> Result<Option<Settings>, String> {
    match (
        env_path(ENV_CERT),
        env_path(ENV_KEY),
        env_path(ENV_CLIENT_CA),
    ) {
        (None, None, None) => Ok(None),
        (Some(cert), Some(key), client_ca) => Ok(Some(Settings {
            cert,
            key,
            client_ca,
        })),
        (None, None, Some(_)) => Err(format!(
            "{ENV_CLIENT_CA} needs native TLS: set {ENV_CERT} and {ENV_KEY} too"
        )),
        _ => Err(format!("set both {ENV_CERT} and {ENV_KEY}, or neither")),
    }
}

/// Build the rustls server configuration (HTTP/2 and HTTP/1.1 via ALPN).
pub fn server_config(s: &Settings) -> Result<ServerConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(&s.cert)
        .and_then(Iterator::collect)
        .map_err(|e| format!("{ENV_CERT} ({}): {e}", s.cert.display()))?;
    if certs.is_empty() {
        return Err(format!(
            "{ENV_CERT} ({}) holds no certificate",
            s.cert.display()
        ));
    }
    let key = PrivateKeyDer::from_pem_file(&s.key)
        .map_err(|e| format!("{ENV_KEY} ({}): {e}", s.key.display()))?;

    let builder = ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS: {e}"))?;
    let builder = match &s.client_ca {
        Some(path) => {
            let mut roots = RootCertStore::empty();
            let cas: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(path)
                .and_then(Iterator::collect)
                .map_err(|e| format!("{ENV_CLIENT_CA} ({}): {e}", path.display()))?;
            let (added, _) = roots.add_parsable_certificates(cas);
            if added == 0 {
                return Err(format!(
                    "{ENV_CLIENT_CA} ({}) holds no usable CA certificate",
                    path.display()
                ));
            }
            let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
                .allow_unauthenticated()
                .build()
                .map_err(|e| format!("{ENV_CLIENT_CA}: {e}"))?;
            builder.with_client_cert_verifier(verifier)
        }
        None => builder.with_no_client_auth(),
    };
    let mut config = builder
        .with_single_cert(certs, key)
        .map_err(|e| format!("{ENV_CERT} / {ENV_KEY}: {e}"))?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

/// The fingerprint of a presented (and, by then, verified) leaf certificate.
#[must_use]
pub fn client_cert(peer: Option<&[CertificateDer<'_>]>) -> Option<ClientCert> {
    peer.and_then(<[_]>::first).map(|leaf| ClientCert {
        fingerprint: hex::encode(Sha256::digest(leaf.as_ref())),
    })
}

/// Serve `app` over TLS on `addr` until `shutdown` resolves, then let open
/// connections finish. Every request carries `ConnectInfo<SocketAddr>`
/// (as `axum::serve` provides) and, when one was verified, a [`ClientCert`].
pub async fn serve(
    app: Router,
    addr: &str,
    config: ServerConfig,
    shutdown: impl Future<Output = ()> + Send,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let graceful = GracefulShutdown::new();
    tracing::info!(addr, "serving HTTPS");
    tokio::pin!(shutdown);
    loop {
        let (tcp, remote) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(c) => c,
                Err(e) => {
                    // Out of file descriptors and the like: back off a little.
                    tracing::warn!(error = %e, "accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            () = &mut shutdown => break,
        };
        let acceptor = acceptor.clone();
        let app = app.clone();
        let watcher = graceful.watcher();
        tokio::spawn(async move {
            let tls = match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    tracing::debug!(error = %e, %remote, "TLS handshake failed");
                    return;
                }
                Err(_) => {
                    tracing::debug!(%remote, "TLS handshake timed out");
                    return;
                }
            };
            let cert = client_cert(tls.get_ref().1.peer_certificates());
            let svc = tower::service_fn(
                move |mut req: axum::extract::Request<hyper::body::Incoming>| {
                    req.extensions_mut().insert(ConnectInfo(remote));
                    if let Some(c) = &cert {
                        req.extensions_mut().insert(c.clone());
                    }
                    app.clone().oneshot(req)
                },
            );
            let builder = Builder::new(TokioExecutor::new());
            let conn = builder
                .serve_connection_with_upgrades(TokioIo::new(tls), TowerToHyperService::new(svc));
            if let Err(e) = watcher.watch(conn.into_owned()).await {
                tracing::debug!(error = %e, %remote, "connection ended with an error");
            }
        });
    }
    drop(listener);
    // Give in-flight requests a bounded time to finish.
    if tokio::time::timeout(Duration::from_secs(30), graceful.shutdown())
        .await
        .is_err()
    {
        tracing::warn!("connections still open after 30 s; exiting anyway");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_need_cert_and_key_together() {
        for v in [ENV_CERT, ENV_KEY, ENV_CLIENT_CA] {
            std::env::remove_var(v);
        }
        assert_eq!(settings_from_env(), Ok(None));
        // Empty values (as Compose passes optional ones) are unset.
        for v in [ENV_CERT, ENV_KEY, ENV_CLIENT_CA] {
            std::env::set_var(v, "");
        }
        assert_eq!(settings_from_env(), Ok(None));
        std::env::set_var(ENV_CLIENT_CA, "/ca.pem");
        assert!(settings_from_env().is_err());
        std::env::set_var(ENV_CERT, "/cert.pem");
        assert!(settings_from_env().is_err());
        std::env::set_var(ENV_KEY, "/key.pem");
        assert_eq!(
            settings_from_env(),
            Ok(Some(Settings {
                cert: "/cert.pem".into(),
                key: "/key.pem".into(),
                client_ca: Some("/ca.pem".into()),
            }))
        );
        for v in [ENV_CERT, ENV_KEY, ENV_CLIENT_CA] {
            std::env::remove_var(v);
        }
    }
}
