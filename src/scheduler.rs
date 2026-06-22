use std::{sync::Arc, time::Duration};

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

use crate::{
    providers::{MovieProvider, SeriesProvider},
    sync::SyncOrchestrator,
};

/// Drives [`SyncOrchestrator::sync`] on a periodic timer and on external
/// triggers, with two guarantees:
///
/// - **single-flight** — the run loop awaits each sync before looping, so two
///   syncs never overlap (which would race on the upsert transactions);
/// - **coalescing** — a burst of triggers (e.g. Sonarr firing a webhook per
///   imported episode) collapses into one sync. Triggers go through a
///   [`Notify`], which stores at most one permit: any number of `notify_one`
///   calls made while a sync is running wake the scheduler exactly once.
///
/// The periodic tick and the startup sync skip the debounce — only the trigger
/// path waits, to widen the window in which a burst is absorbed before running.
pub struct SyncScheduler<S, M> {
    orchestrator: Arc<SyncOrchestrator<S, M>>,
    interval: Duration,
    debounce: Duration,
    trigger: Arc<Notify>,
}

impl<S, M> SyncScheduler<S, M>
where
    S: SeriesProvider + Send + Sync + 'static,
    M: MovieProvider + Send + Sync + 'static,
{
    pub fn new(
        orchestrator: Arc<SyncOrchestrator<S, M>>,
        interval: Duration,
        debounce: Duration,
        trigger: Arc<Notify>,
    ) -> Self {
        Self {
            orchestrator,
            interval,
            debounce,
            trigger,
        }
    }

    /// Run one sync, swallowing errors. A failed sync must not take down the
    /// daemon — it keeps serving work already in the DB and the next tick or
    /// trigger retries. This is what makes the startup sync non-fatal.
    async fn run_sync(&self) {
        if let Err(e) = self.orchestrator.sync().await {
            warn!(error = %e, "library sync failed; will retry on next tick/trigger");
        }
    }

    #[instrument(skip_all)]
    pub async fn run(self, token: CancellationToken) -> anyhow::Result<()> {
        // Immediate, non-blocking startup sync — this replaces the old blocking
        // `sync().await?` in main. On a fresh DB it populates the pipeline; on a
        // restart the listener's catch-up already resumed existing work, so this
        // just reconciles against the providers.
        info!("running startup sync");
        self.run_sync().await;

        // The periodic timer is disabled when the configured cadence is zero:
        // keep an optional ticker and only select on it when present.
        let mut ticker = (!self.interval.is_zero()).then(|| {
            let mut t = tokio::time::interval(self.interval);
            t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            t
        });
        // The first `interval` tick is immediate; consume it so the periodic
        // cadence starts one full period *after* the startup sync above.
        if let Some(t) = ticker.as_mut() {
            t.tick().await;
        }

        loop {
            tokio::select! {
                biased;
                _ = token.cancelled() => {
                    info!("sync scheduler cancelled, shutting down");
                    return Ok(());
                }
                _ = async { ticker.as_mut().unwrap().tick().await }, if ticker.is_some() => {
                    info!("periodic sync tick");
                    self.run_sync().await;
                }
                _ = self.trigger.notified() => {
                    // Debounce so a burst of triggers is absorbed into one run.
                    tokio::time::sleep(self.debounce).await;
                    info!("triggered sync");
                    self.run_sync().await;
                }
            }
        }
    }
}
