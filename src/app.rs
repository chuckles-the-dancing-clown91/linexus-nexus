use async_trait::async_trait;
use loco_rs::{
    app::{AppContext, Hooks, Initializer},
    bgworker::{BackgroundWorker, Queue},
    boot::{create_app, BootResult, StartMode},
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
        seed_runtime(ctx).await
    }

    async fn initializers(_ctx: &AppContext) -> Result<Vec<Box<dyn Initializer>>> {
        Ok(vec![])
    }

    fn routes(_ctx: &AppContext) -> AppRoutes {
        AppRoutes::with_default_routes() // controller routes below
            .add_route(controllers::auth::routes())
            .add_route(controllers::tasks::routes())
            .add_route(controllers::agents::routes())
            .add_route(controllers::roles::routes())
            .add_route(controllers::nexus::routes())
            .add_route(controllers::housing::routes())
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
