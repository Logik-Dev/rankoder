use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument};
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
    let _guard = init_tracing();

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

    let workflow_orchestrator = WorkflowOrchestrator::new(
        rx,
        store.clone(),
        Arc::new(FFmpeg),
        analysis_orchestrator,
        approval_orchestrator.clone(),
    );

    let postgres_listener = PostgresListener::new(pool.clone(), store.clone(), tx);

    let provider = JellyfinProvider::new(&cfg.jellyfin_url, &cfg.jellyfin_api_key)?;

    let sync_orchestrator = SyncOrchestrator::new(provider.clone(), provider, store.clone());
    sync_orchestrator.sync().await?;

    info!("sync complete, waiting for Ctrl+C to stop");

    let token = CancellationToken::new();
    let mut join_set = JoinSet::new();

    join_set.spawn(postgres_listener.listen(token.child_token()));
    join_set.spawn(workflow_orchestrator.run(token.child_token()));
    join_set.spawn(
        approval_orchestrator
            .clone()
            .run_response_listener(token.child_token()),
    );
    join_set.spawn(
        approval_orchestrator
            .clone()
            .run_approval_feeder(token.child_token(), cfg.approval_max_pending),
    );
    join_set.spawn(approval_orchestrator.run_stale_checker(token.child_token(), 5));

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("shutting down");
            token.cancel();
        }
        res = join_set.join_next() => {
            match res {
                Some(Ok(Err(task_error))) => error!("task failed: {task_error}"),
                Some(Err(join_error)) => error!("task panicked: {join_error}"),
                Some(Ok(Ok(()))) => info!("task completed normally"),
                None => {}
            }
            token.cancel();
        }
    }

    while let Some(res) = join_set.join_next().await {
        match res {
            Ok(Err(task_error)) => error!("task failed during drain: {task_error}"),
            Err(join_error) => error!("task panicked during drain: {join_error}"),
            Ok(Ok(())) => {}
        }
    }

    Ok(())
}

fn init_tracing() -> tracing_appender::non_blocking::WorkerGuard {
    let stdout_layer = fmt::layer().compact().with_target(false);

    let file_appender = tracing_appender::rolling::daily("logs", "rankoder.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    let json_layer = fmt::layer().json().with_writer(non_blocking);

    let _ = tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(stdout_layer)
        .with(json_layer)
        .try_init();

    guard
}
