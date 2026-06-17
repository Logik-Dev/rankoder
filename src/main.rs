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
    models::workflow::WorkflowStateTag,
    notification::{StatusNotifier, mqtt::MqttNotifier, reporter::StatusReporter},
    probe::FFmpeg,
    providers::{JellyfinProvider, MovieNotifier, RadarrClient, SeriesNotifier, SonarrClient},
    store::MediaStore,
    sync::SyncOrchestrator,
    transcode::orchestrator::{MediaNotifiers, TranscodeOrchestrator},
    transcode::reaper::RetentionReaper,
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
mod transcode;
mod workflow;

#[tokio::main]
#[instrument(err)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _guard = init_tracing();

    let cfg = AppConfig::from_env()?;

    // Cover the workflow's worker pool (available_parallelism) plus the
    // background tasks (feeder, stale checker, response handler).
    // PgListener has its own dedicated connection, not counted here.
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get());
    let max_connections = (workers + 5) as u32;

    let pool = PgPoolOptions::new()
        .max_connections(max_connections)
        // Slow-acquire spikes are expected during catch-up bursts; keep them at
        // trace level instead of warn so they don't flood the logs.
        //.acquire_slow_level(log::LevelFilter::Trace)
        .connect(&cfg.database_url)
        .await?;

    if cfg.auto_migrate {
        sqlx::migrate!("./migrations").run(&pool).await?;
        info!("migrations applied");
    }

    let (tx, rx) = mpsc::channel(100);
    let (tx_t, rx_t) = mpsc::channel(100);
    let store = Arc::new(MediaStore::new(pool.clone()));

    let decision_service = TakeTranscodeDecisionService::new(
        cfg.min_size_per_hour_gb,
        cfg.min_bpp,
        cfg.min_bpp_hevc,
        cfg.min_compression_potential,
    );
    let analysis_orchestrator = AnalysisOrchestrator::new(store.clone(), decision_service);

    let notifier = Arc::new(MqttNotifier::new(
        &cfg.mqtt_host,
        cfg.mqtt_port,
        &cfg.mqtt_client_id,
    ));
    // The same MQTT notifier also drives operator-facing status/failure topics.
    let status_notifier: Arc<dyn StatusNotifier> = notifier.clone();
    let approval_orchestrator = Arc::new(ApprovalOrchestrator::new(store.clone(), notifier));

    let workflow_orchestrator = WorkflowOrchestrator::new(
        rx,
        store.clone(),
        Arc::new(FFmpeg::new(3)),
        analysis_orchestrator,
        approval_orchestrator.clone(),
        tx_t.clone(),
    );

    let postgres_listener = PostgresListener::new(cfg.database_url.clone(), store.clone(), tx);

    let provider = JellyfinProvider::new(&cfg.jellyfin_url, &cfg.jellyfin_api_key)?;

    let sync_orchestrator = SyncOrchestrator::new(provider.clone(), provider, store.clone());
    sync_orchestrator.sync().await?;

    info!("sync complete, waiting for Ctrl+C to stop");

    // Detect encoder at startup
    info!("detecting HEVC encoder...");
    let override_enc = if cfg.transcode_encoder_override == "auto" {
        None
    } else {
        Some(
            transcode::encoder::Encoder::from_str(&cfg.transcode_encoder_override).ok_or_else(
                || {
                    format!(
                        "invalid TRANSCODE_ENCODER: {}",
                        cfg.transcode_encoder_override
                    )
                },
            )?,
        )
    };
    let encoder = transcode::detect::detect_encoder(override_enc).await?;
    info!(?encoder, "HEVC encoder selected");

    // Optional: refresh the media manager after each transcode so it picks up
    // the new file. Each is disabled (no-op) unless both URL and API key are
    // configured — Radarr for movies, Sonarr for series.
    let movie_notifier: Option<Arc<dyn MovieNotifier>> =
        match (&cfg.radarr_url, &cfg.radarr_api_key) {
            (Some(url), Some(api_key)) => {
                info!("Radarr notifier enabled");
                Some(Arc::new(RadarrClient::new(url, api_key)?))
            }
            _ => {
                info!("Radarr not configured, skipping movie refresh");
                None
            }
        };

    let series_notifier: Option<Arc<dyn SeriesNotifier>> =
        match (&cfg.sonarr_url, &cfg.sonarr_api_key) {
            (Some(url), Some(api_key)) => {
                info!("Sonarr notifier enabled");
                Some(Arc::new(SonarrClient::new(url, api_key)?))
            }
            _ => {
                info!("Sonarr not configured, skipping series refresh");
                None
            }
        };

    let transcode_orchestrator = TranscodeOrchestrator::new(
        rx_t,
        store.clone(),
        encoder,
        cfg.transcode_tmp_dir.into(),
        cfg.transcode_retention_dir.into(),
        cfg.transcode_min_size_reduction,
        MediaNotifiers {
            movie: movie_notifier,
            series: series_notifier,
        },
    );

    // Recovery: re-enqueue files stuck in Transcoding state.
    // Now largely redundant: the transcode orchestrator's periodic stale
    // re-queue fires its first tick immediately on startup and recovers the
    // same files (deduped against what's already pending/in-flight). Kept only
    // for an immediate, per-file-logged recovery; safe to remove.
    let stuck_ids = store
        .fetch_files_in_state(WorkflowStateTag::Transcoding)
        .await?;
    for id in stuck_ids {
        tx_t.send(id).await?;
        info!(?id, "recovered stuck transcode file");
    }

    let token = CancellationToken::new();
    let mut join_set = JoinSet::new();

    join_set.spawn(postgres_listener.listen(token.child_token()));
    join_set.spawn(workflow_orchestrator.run(token.child_token()));
    join_set.spawn(transcode_orchestrator.run(token.child_token()));
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

    let reaper = RetentionReaper::new(store.clone(), cfg.transcode_retention_days);
    join_set.spawn(reaper.run(token.child_token()));

    let status_reporter = StatusReporter::new(store.clone(), status_notifier);
    join_set.spawn(status_reporter.run(token.child_token()));

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
