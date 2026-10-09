//! What a non-development Nexus refuses to start without.
//!
//! [`check`] is pure (it reads the environment through a lookup function) so
//! the rules are unit-tested; [`crate::app`] runs it in `before_run` and
//! refuses to start on any error. "Strict" is every environment other than
//! development and test — production, and any custom one such as staging.
//!
//! Strict errors: `NEXUS_SYSTEM_TOKEN` unset, shorter than 32 characters or
//! a known development value; `NEXUS_SECRET_KEY` unset, short or the
//! development material; a JWT secret (`NEXUS_JWT_SECRET` in
//! `config/production.yaml`) shorter than 32 characters;
//! `NEXUS_REQUIRE_OPERATOR_CERT` without `NEXUS_TLS_CLIENT_CA` (it would
//! refuse every operator). Errors everywhere: half a TLS configuration, an
//! unparseable `NEXUS_SIGNING_KEY`. Warnings: `NEXUS_PUBLIC_URL` unset or not
//! `https://`.

use crate::middleware::{client_cert, system_token};
use crate::{secrets, signing, tls};

/// Minimum length of the root token, the sealing key and the JWT secret.
pub const MIN_SECRET_LEN: usize = 32;

/// Values that must never guard a real deployment.
const KNOWN_DEV_TOKENS: [&str; 4] = [
    system_token::DEV_ROOT_TOKEN,
    "changeme",
    "change-me",
    "nexus-system-token",
];

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Findings {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

fn nonempty(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Check the environment. `strict` is true outside development and test;
/// `jwt_secret` is the configured JWT secret, if any.
pub fn check(
    strict: bool,
    var: &dyn Fn(&str) -> Option<String>,
    jwt_secret: Option<&str>,
) -> Findings {
    let mut f = Findings::default();
    let get = |k: &str| nonempty(var(k));

    if strict {
        match get(system_token::ENV_ROOT_TOKEN) {
            None => f
                .errors
                .push(format!("{} is not set", system_token::ENV_ROOT_TOKEN)),
            Some(t) if KNOWN_DEV_TOKENS.iter().any(|d| d.eq_ignore_ascii_case(&t)) => {
                f.errors.push(format!(
                    "{} is a known development value",
                    system_token::ENV_ROOT_TOKEN
                ))
            }
            Some(t) if t.len() < MIN_SECRET_LEN => f.errors.push(format!(
                "{} must be at least {MIN_SECRET_LEN} characters",
                system_token::ENV_ROOT_TOKEN
            )),
            Some(_) => {}
        }
        match get(secrets::ENV_SECRET_KEY) {
            None => f
                .errors
                .push(format!("{} is not set", secrets::ENV_SECRET_KEY)),
            Some(k) if k == secrets::DEV_KEY_MATERIAL => f.errors.push(format!(
                "{} is the development key",
                secrets::ENV_SECRET_KEY
            )),
            Some(k) if k.len() < MIN_SECRET_LEN => f.errors.push(format!(
                "{} must be at least {MIN_SECRET_LEN} characters",
                secrets::ENV_SECRET_KEY
            )),
            Some(_) => {}
        }
        if jwt_secret.map_or(0, |s| s.trim().len()) < MIN_SECRET_LEN {
            f.errors.push(format!(
                "the JWT secret (NEXUS_JWT_SECRET) must be at least {MIN_SECRET_LEN} characters"
            ));
        }
    }

    if client_cert::required_value(var(client_cert::ENV_REQUIRE).as_deref())
        && get(tls::ENV_CLIENT_CA).is_none()
    {
        let msg = format!(
            "{} is on but {} is not set: every operator request would be refused",
            client_cert::ENV_REQUIRE,
            tls::ENV_CLIENT_CA
        );
        if strict {
            f.errors.push(msg);
        } else {
            f.warnings.push(msg);
        }
    }

    let cert = get(tls::ENV_CERT).is_some();
    let key = get(tls::ENV_KEY).is_some();
    if cert != key {
        f.errors.push(format!(
            "set both {} and {}, or neither",
            tls::ENV_CERT,
            tls::ENV_KEY
        ));
    } else if !cert && get(tls::ENV_CLIENT_CA).is_some() {
        f.errors.push(format!(
            "{} needs native TLS: set {} and {} too",
            tls::ENV_CLIENT_CA,
            tls::ENV_CERT,
            tls::ENV_KEY
        ));
    }

    if let Some(seed) = get(signing::ENV_SIGNING_KEY) {
        if let Err(e) = signing::parse_seed(&seed) {
            f.errors.push(e);
        }
    }

    match get("NEXUS_PUBLIC_URL") {
        Some(u) if u.starts_with("https://") => {}
        Some(_) if strict => f
            .warnings
            .push("NEXUS_PUBLIC_URL is not https: agents installed from it fetch the installer and pinned key in the clear".into()),
        None if strict => f
            .warnings
            .push("NEXUS_PUBLIC_URL is not set: droplets created with enrollAgent cannot call home".into()),
        _ => {}
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const GOOD_TOKEN: &str = "0123456789abcdef0123456789abcdef-root";
    const GOOD_KEY: &str = "fedcba9876543210fedcba9876543210-seal";
    const GOOD_JWT: &str = "jwt-secret-jwt-secret-jwt-secret-jwt";

    fn run(strict: bool, vars: &[(&str, &str)], jwt: Option<&str>) -> Findings {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        check(strict, &|k| map.get(k).cloned(), jwt)
    }

    fn good() -> Vec<(&'static str, &'static str)> {
        vec![
            ("NEXUS_SYSTEM_TOKEN", GOOD_TOKEN),
            ("NEXUS_SECRET_KEY", GOOD_KEY),
            ("NEXUS_PUBLIC_URL", "https://nexus.example.com"),
        ]
    }

    #[test]
    fn a_complete_production_environment_passes() {
        assert_eq!(run(true, &good(), Some(GOOD_JWT)), Findings::default());
    }

    #[test]
    fn production_refuses_missing_short_or_development_secrets() {
        let f = run(true, &[], None);
        assert_eq!(f.errors.len(), 3, "{f:?}");
        assert!(f.errors[0].contains("NEXUS_SYSTEM_TOKEN is not set"));
        assert!(f.errors[1].contains("NEXUS_SECRET_KEY is not set"));
        assert!(f.errors[2].contains("JWT"));

        for bad in ["short-token", system_token::DEV_ROOT_TOKEN] {
            let mut vars = good();
            vars[0] = ("NEXUS_SYSTEM_TOKEN", bad);
            let f = run(true, &vars, Some(GOOD_JWT));
            assert_eq!(f.errors.len(), 1, "{bad}: {f:?}");
            assert!(f.errors[0].starts_with("NEXUS_SYSTEM_TOKEN"));
        }
        for bad in ["short", secrets::DEV_KEY_MATERIAL] {
            let mut vars = good();
            vars[1] = ("NEXUS_SECRET_KEY", bad);
            let f = run(true, &vars, Some(GOOD_JWT));
            assert_eq!(f.errors.len(), 1, "{bad}: {f:?}");
            assert!(f.errors[0].starts_with("NEXUS_SECRET_KEY"));
        }
        // The development JWT secret from config/development.yaml is short.
        assert_eq!(
            run(true, &good(), Some("MSx8GmitvECyjdhLQv5a"))
                .errors
                .len(),
            1
        );
    }

    #[test]
    fn development_is_relaxed_but_broken_settings_still_fail() {
        assert_eq!(run(false, &[], None), Findings::default());
        let f = run(false, &[("NEXUS_TLS_CERT", "/c.pem")], None);
        assert_eq!(f.errors.len(), 1);
        let f = run(false, &[("NEXUS_SIGNING_KEY", "AAAA")], None);
        assert_eq!(f.errors.len(), 1);
        // Requiring operator certificates without a CA only warns here…
        let f = run(false, &[("NEXUS_REQUIRE_OPERATOR_CERT", "1")], None);
        assert_eq!((f.errors.len(), f.warnings.len()), (0, 1));
        // …and refuses in production.
        let mut vars = good();
        vars.push(("NEXUS_REQUIRE_OPERATOR_CERT", "1"));
        assert_eq!(run(true, &vars, Some(GOOD_JWT)).errors.len(), 1);
    }

    #[test]
    fn plain_http_public_url_warns() {
        let mut vars = good();
        vars[2] = ("NEXUS_PUBLIC_URL", "http://nexus.example.com");
        let f = run(true, &vars, Some(GOOD_JWT));
        assert!(f.errors.is_empty());
        assert_eq!(f.warnings.len(), 1);
    }
}
