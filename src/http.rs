use std::{net::SocketAddr, sync::Arc};

use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use tokio::{net::TcpListener, sync::Notify};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Header carrying the shared secret on webhook calls.
const TOKEN_HEADER: &str = "x-rankoder-token";

#[derive(Clone)]
struct WebhookState {
    trigger: Arc<Notify>,
    token: Arc<String>,
}

/// Run the webhook HTTP server until `cancel` fires.
///
/// Exposes:
/// - `POST /sync` — requires the `X-Rankoder-Token` header; pings the sync
///   scheduler and returns `202`. The body is ignored: any source (Radarr,
///   Sonarr, Jellyfin) just nudges a full, debounced re-sync.
/// - `GET /healthz` — unauthenticated liveness probe.
pub async fn serve(
    bind: String,
    token: String,
    trigger: Arc<Notify>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let state = WebhookState {
        trigger,
        token: Arc::new(token),
    };

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/sync", post(sync))
        .with_state(state);

    let addr: SocketAddr = bind.parse()?;
    let listener = TcpListener::bind(addr).await?;
    info!(%addr, "webhook server listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(async move { cancel.cancelled().await })
        .await?;
    Ok(())
}

async fn healthz() -> StatusCode {
    StatusCode::OK
}

async fn sync(State(state): State<WebhookState>, headers: HeaderMap) -> StatusCode {
    let provided = headers.get(TOKEN_HEADER).and_then(|v| v.to_str().ok());
    if provided != Some(state.token.as_str()) {
        warn!("webhook /sync rejected: missing or invalid token");
        return StatusCode::UNAUTHORIZED;
    }
    // `notify_one` coalesces: repeated calls while the scheduler is busy collapse
    // into a single wake-up, so a burst of webhooks triggers one debounced sync.
    state.trigger.notify_one();
    info!("webhook /sync accepted, sync triggered");
    StatusCode::ACCEPTED
}
