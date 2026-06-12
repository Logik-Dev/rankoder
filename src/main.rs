use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;
use tokio::sync::mpsc;
use tracing::{info, instrument};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use crate::{
    analysis::{AnalysisOrchestrator, TakeTranscodeDecisionService},
    approval::ApprovalOrchestrator,
    config::AppConfig,
    listener::PostgresListener,
    notification::mqtt::MqttNotifier,
    probe::FFmpeg,
    providers::JellyfinProvider,
    store::MediaStore,
    sync::SyncOrchestrator,
    workflow::WorkflowOrchestrator,
};

mod analysis;
mod approval;
mod config;
mod listener;
pub mod models;
mod notification;
mod probe;
pub mod providers;
pub mod store;
mod sync;
mod workflow;

#[tokio::main]
#[instrument(err)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let cfg = AppConfig::from_env()?;

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&cfg.database_url)
        .await?;

    if cfg.auto_migrate {
        sqlx::migrate!("./migrations").run(&pool).await?;
        info!("migrations applied");
    }

    let (tx, rx) = mpsc::channel(100);
    let store = Arc::new(MediaStore::new(pool.clone()));

    let decision_service = TakeTranscodeDecisionService::new(
        cfg.min_size_per_hour_gb,
        cfg.min_bpp,
        cfg.min_compression_potential,
    );
    let analysis_orchestrator = AnalysisOrchestrator::new(store.clone(), decision_service);

    let notifier = Arc::new(MqttNotifier::new(
        &cfg.mqtt_host,
        cfg.mqtt_port,
        &cfg.mqtt_client_id,
    ));
    let approval_orchestrator = Arc::new(ApprovalOrchestrator::new(store.clone(), notifier));
    let approval_handle = tokio::spawn(Arc::clone(&approval_orchestrator).run_response_listener());

    let workflow_orchestrator = WorkflowOrchestrator::new(
        rx,
        store.clone(),
        Box::new(FFmpeg),
        analysis_orchestrator,
        approval_orchestrator,
    );
    let workflow_handle = tokio::spawn(workflow_orchestrator.run());

    let postgres_listener = PostgresListener::new(pool.clone(), store.clone(), tx);
    let listener_handle = tokio::spawn(postgres_listener.listen());

    let provider = JellyfinProvider::new(&cfg.jellyfin_url, &cfg.jellyfin_api_key)?;

    let sync_orchestrator = SyncOrchestrator::new(provider.clone(), provider, store.clone());
    sync_orchestrator.sync().await?;

    info!("sync complete, waiting for Ctrl+C to stop");

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("shutting down");
        }
        res = listener_handle => {
            info!("listener stopped: {:?}", res);
        }
        res = workflow_handle => {
            info!("workflow stopped: {:?}", res);
        }
        res = approval_handle => {
            info!("approval listener stopped: {:?}", res);
        }
    }

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
