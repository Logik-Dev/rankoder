use sqlx::postgres::PgPoolOptions;
use tracing::{info, instrument};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use crate::{
    config::Config, orchestrator::SyncOrchestrator, providers::JellyfinProvider, store::MediaStore,
};

mod config;
pub mod models;
mod orchestrator;
pub mod providers;
pub mod store;

#[tokio::main]
#[instrument(err)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let cfg = Config::from_env()?;

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&std::env::var("DATABASE_URL")?)
        .await?;

    if std::env::var("AUTO_MIGRATE").is_ok_and(|v| v == "1") {
        sqlx::migrate!("./migrations").run(&pool).await?;
        info!("migrations applied");
    }

    let provider = JellyfinProvider::new(&cfg.jellyfin_url, &cfg.jellyfin_api_key)?;

    let store = MediaStore::new(pool);
    let orchestrator = SyncOrchestrator::new(provider.clone(), provider, store);
    orchestrator.sync().await?;

    Ok(())
}

fn init_tracing() {
    let stdout_layer = fmt::layer().compact().with_target(false);

    let file_appender = tracing_appender::rolling::daily("logs", "rankoder.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    std::mem::forget(guard);
    let json_layer = fmt::layer().json().with_writer(non_blocking);

    let _ = tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(stdout_layer)
        .with(json_layer)
        .try_init();
}
