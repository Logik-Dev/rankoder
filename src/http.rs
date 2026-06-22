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

use crate::{store::MediaStore, ui};

/// Header carrying the shared secret on webhook calls.
const TOKEN_HEADER: &str = "x-rankoder-token";

#[derive(Clone)]
struct WebhookState {
    trigger: Arc<Notify>,
    token: Arc<String>,
}

/// Run the HTTP server until `cancel` fires. It hosts two things on one listener:
///
/// - the operator **UI** (`GET /`, `/static/*`) and a `GET /healthz` probe —
///   always on, unauthenticated, meant to sit behind a reverse proxy;
/// - the sync **webhook** (`POST /sync`) — mounted only when a `token` is given.
///   It requires the `X-Rankoder-Token` header and nudges a debounced re-sync;
///   the body is ignored, so any caller (Radarr, Sonarr, Jellyfin) just pings it.
///
/// Decoupling the webhook from the bind (it used to be mandatory) lets the UI be
/// served without exposing a sync endpoint.
pub async fn serve(
    bind: String,
    token: Option<String>,
    store: Arc<MediaStore>,
    trigger: Arc<Notify>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let mut app = Router::new()
        .route("/healthz", get(healthz))
        .merge(ui::router(store));

    match token {
        Some(token) => {
            let state = WebhookState {
                trigger,
                token: Arc::new(token),
            };
            app = app.merge(Router::new().route("/sync", post(sync)).with_state(state));
            info!("sync webhook enabled (POST /sync)");
        }
        None => info!("sync webhook disabled (WEBHOOK_TOKEN unset); UI only"),
    }

    let addr: SocketAddr = bind.parse()?;
    let listener = TcpListener::bind(addr).await?;
    info!(%addr, "http server listening");

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
