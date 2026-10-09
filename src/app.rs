use async_trait::async_trait;
use axum::Router as AxumRouter;
use loco_rs::{
    app::{AppContext, Hooks, Initializer},
    bgworker::{BackgroundWorker, Queue},
    boot::{create_app, shutdown_signal, BootResult, ServeParams, StartMode},
    config::Config,
    controller::AppRoutes,
    db::{self, truncate_table},
    environment::Environment,
    task::Tasks,
    Result,
};
use migration::Migrator;
use sea_orm::IntoActiveModel;
use std::path::Path;

#[allow(unused_imports)]
use crate::{
    controllers,
    middleware::rbac,
    models::{
        _entities::users,
        plans, roles as roles_model,
        users::{Model as UserModel, RegisterParams},
    },
    tasks,
    workers::downloader::DownloadWorker,
};

/// Default development admin credentials, seeded at boot in the development
/// environment so the stack is usable end-to-end without manual setup.
const DEV_ADMIN_EMAIL: &str = "admin@linexus.local";
const DEV_ADMIN_PASSWORD: &str = "admin1234";
const DEV_ADMIN_NAME: &str = "Linexus Admin";

/// Seed the default RBAC roles and (in development) an admin user.
///
/// Idempotent: roles and the admin user are only created if absent, so this is
/// safe to run on every boot.
async fn seed_runtime(ctx: &AppContext) -> Result<()> {
    // Always ensure the default system roles exist.
    roles_model::Model::seed_defaults(&ctx.db).await?;

    // Only seed a default admin user in development.
    if !matches!(ctx.environment, Environment::Development) {
        return Ok(());
    }

    if UserModel::find_by_email(&ctx.db, DEV_ADMIN_EMAIL)
        .await
        .is_ok()
    {
        return Ok(());
    }

    let admin = UserModel::create_with_password(
        &ctx.db,
        &RegisterParams {
            email: DEV_ADMIN_EMAIL.to_string(),
            password: DEV_ADMIN_PASSWORD.to_string(),
            name: DEV_ADMIN_NAME.to_string(),
        },
    )
    .await?;

    // Mark verified, put on the enterprise plan, and grant the admin role.
    let admin = admin.into_active_model().verified(&ctx.db).await?;
    let admin = admin
        .into_active_model()
        .set_plan(&ctx.db, "enterprise")
        .await?;
    rbac::assign_role(&ctx.db, admin.id, plans::default_role("enterprise")).await?;

    tracing::info!(
        email = DEV_ADMIN_EMAIL,
        "seeded development admin user (plan=enterprise, role=admin)"
    );

    Ok(())
}

/// Whether `environment` is development or test (everything else is held
/// to the production rules in [`crate::boot_checks`]).
fn relaxed(environment: &Environment) -> bool {
    matches!(environment, Environment::Development | Environment::Test)
}

/// Refuse to start on an unsafe configuration, log what only deserves a
/// warning, then make sure the plan signing key exists and bootstrap
/// provider credentials from the environment.
async fn prepare_runtime(ctx: &AppContext) -> Result<()> {
    let jwt = ctx
        .config
        .auth
        .as_ref()
        .and_then(|a| a.jwt.as_ref())
        .map(|j| j.secret.clone());
    let findings = crate::boot_checks::check(
        !relaxed(&ctx.environment),
        &|k| std::env::var(k).ok(),
        jwt.as_deref(),
    );
    for w in &findings.warnings {
        tracing::warn!("{w}");
    }
    if !findings.errors.is_empty() {
        for e in &findings.errors {
            tracing::error!("{e}");
        }
        return Err(loco_rs::Error::Message(format!(
            "refusing to start ({} environment): {}",
            ctx.environment,
            findings.errors.join("; ")
        )));
    }

    let signer = crate::signing::init(ctx)
        .await
        .map_err(|e| loco_rs::Error::Message(format!("refusing to start: {e}")))?;
    tracing::info!(key_id = %signer.key_id(), "plan signing key ready");

    crate::providers::bootstrap_from_env(ctx).await;
    Ok(())
}

pub struct App;
#[async_trait]
impl Hooks for App {
    fn app_name() -> &'static str {
        env!("CARGO_CRATE_NAME")
    }

    fn app_version() -> String {
        format!(
            "{} ({})",
            env!("CARGO_PKG_VERSION"),
            option_env!("BUILD_SHA")
                .or(option_env!("GITHUB_SHA"))
                .unwrap_or("dev")
        )
    }

    async fn boot(
        mode: StartMode,
        environment: &Environment,
        config: Config,
    ) -> Result<BootResult> {
        create_app::<Self, Migrator>(mode, environment, config).await
    }

    /// Runs after the database has migrated and before the server/worker start.
    /// This is the hook the CLI `start` path actually invokes (it calls
    /// `create_app` directly, bypassing `boot`), so seeding lives here to
    /// guarantee it runs. Seeds default roles + a dev admin so auth and
    /// permissions work out of the box.
    async fn before_run(ctx: &AppContext) -> Result<()> {
        prepare_runtime(ctx).await?;
        seed_runtime(ctx).await
    }

    /// Every request passes the operator client-certificate rule
    /// ([`crate::middleware::client_cert`]).
    async fn after_routes(router: AxumRouter, _ctx: &AppContext) -> Result<AxumRouter> {
        Ok(router.layer(axum::middleware::from_fn(
            crate::middleware::client_cert::guard,
        )))
    }

    /// Plain HTTP as Loco serves it, or native HTTPS (optionally asking for
    /// client certificates) when `NEXUS_TLS_CERT` / `NEXUS_TLS_KEY` are set.
    async fn serve(app: AxumRouter, ctx: &AppContext, serve_params: &ServeParams) -> Result<()> {
        let addr = format!("{}:{}", serve_params.binding, serve_params.port);
        let shutdown_ctx = ctx.clone();
        let shutdown = async move {
            shutdown_signal().await;
            tracing::info!("shutting down...");
            Self::on_shutdown(&shutdown_ctx).await;
        };
        let tls = crate::tls::settings_from_env().map_err(loco_rs::Error::Message)?;
        let Some(tls) = tls else {
            let listener = tokio::net::TcpListener::bind(&addr).await?;
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown)
            .await?;
            return Ok(());
        };
        let config = crate::tls::server_config(&tls).map_err(loco_rs::Error::Message)?;
        tracing::info!(client_ca = tls.client_ca.is_some(), "native TLS enabled");
        crate::tls::serve(app, &addr, config, shutdown).await?;
        Ok(())
    }

    async fn initializers(_ctx: &AppContext) -> Result<Vec<Box<dyn Initializer>>> {
        Ok(vec![Box::new(crate::initializers::task_sweep::TaskSweep)])
    }

    fn routes(_ctx: &AppContext) -> AppRoutes {
        AppRoutes::with_default_routes() // controller routes below
            .add_route(controllers::auth::routes())
            .add_route(controllers::tasks::routes())
            .add_route(controllers::agents::routes())
            .add_route(controllers::roles::routes())
            .add_route(controllers::nexus::routes())
            .add_route(controllers::housing::routes())
            .add_route(controllers::gateway::routes())
            .add_route(controllers::enrollment::routes())
            .add_route(controllers::providers::routes())
            .add_route(controllers::dns::routes())
            .add_route(controllers::domains::routes())
            .add_route(controllers::cloud::routes())
            .add_route(controllers::install::routes())
    }
    async fn connect_workers(ctx: &AppContext, queue: &Queue) -> Result<()> {
        queue.register(DownloadWorker::build(ctx)).await?;
        Ok(())
    }

    #[allow(unused_variables)]
    fn register_tasks(tasks: &mut Tasks) {
        // tasks-inject (do not remove)
    }
    async fn truncate(ctx: &AppContext) -> Result<()> {
        truncate_table(&ctx.db, users::Entity).await?;
        Ok(())
    }
    async fn seed(ctx: &AppContext, base: &Path) -> Result<()> {
        db::seed::<users::ActiveModel>(&ctx.db, &base.join("users.yaml").display().to_string())
            .await?;
        Ok(())
    }
}
