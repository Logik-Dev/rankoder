//! One-shot maintenance passes run at startup behind env flags. Each is
//! self-contained and idempotent so leaving a flag enabled is harmless; the
//! recommended workflow is still set -> run once -> unset.

use std::path::Path;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

use crate::store::MediaStore;
use crate::transcode::vmaf;

/// Measure VMAF for `done` files that predate the quality gate, while their
/// originals are still in retention. Idempotent — already-scored files fall out
/// of the store query. Best-effort per file: a measurement failure is logged
/// and skipped. Runs sequentially so it never starves live transcodes of CPU,
/// and checks the cancellation token between files for a clean shutdown.
#[instrument(skip_all)]
pub async fn run_vmaf_backfill(
    store: Arc<MediaStore>,
    n_subsample: u32,
    n_threads: usize,
    token: CancellationToken,
) {
    let files = match store.fetch_done_files_missing_vmaf().await {
        Ok(f) => f,
        Err(e) => {
            warn!(%e, "vmaf backfill: failed to fetch candidates");
            return;
        }
    };

    if files.is_empty() {
        info!("vmaf backfill: nothing to do");
        return;
    }

    let total = files.len();
    info!(total, "vmaf backfill: starting");

    for (i, (id, original, transcoded)) in files.into_iter().enumerate() {
        if token.is_cancelled() {
            info!(scored = i, total, "vmaf backfill: cancelled");
            return;
        }

        match vmaf::compute_vmaf(
            Path::new(&original),
            Path::new(&transcoded),
            n_subsample,
            n_threads,
        )
        .await
        {
            Ok(score) => {
                if let Err(e) = store.record_vmaf(&id, score).await {
                    warn!(%e, ?id, "vmaf backfill: failed to record score");
                } else {
                    info!(?id, vmaf = %score, progress = i + 1, total, "vmaf backfill: scored");
                }
            }
            Err(e) => {
                warn!(%e, ?id, "vmaf backfill: measurement failed, skipping");
            }
        }
    }

    info!(total, "vmaf backfill: complete");
}

/// Re-enqueue quality-rejected files after `MIN_VMAF` was lowered: flip eligible
/// `skipped` rows back to `transcoding`. The transcode orchestrator's stale
/// re-queue (first tick fires immediately at startup) polls the DB for
/// `transcoding` files and picks them up on its own — so this only has to touch
/// the state, no channel plumbing or ordering dependency. Only files whose
/// previously measured score clears the current threshold are touched, keeping
/// this safe and idempotent.
#[instrument(skip_all)]
pub async fn run_requeue_quality_skips(
    store: Arc<MediaStore>,
    min_vmaf: f64,
) -> anyhow::Result<()> {
    let ids = store.requeue_quality_skips(min_vmaf).await?;
    info!(
        count = ids.len(),
        min_vmaf, "requeue quality skips: flipped to transcoding for re-encode"
    );
    Ok(())
}
