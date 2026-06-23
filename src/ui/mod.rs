use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    http::header,
    response::{Html, IntoResponse},
    routing::get,
};

use crate::store::MediaStore;

mod views;

/// Read-only operator dashboard, served from the same HTTP listener as the sync
/// webhook. Reads straight through the existing `MediaStore` pool — no separate
/// read-only role — and renders server-side (maud), so there is no JS build and
/// the whole UI ships inside the binary. Auth is delegated to the reverse proxy
/// in front of the loopback bind; the UI itself is unauthenticated.
pub fn router(store: Arc<MediaStore>) -> Router {
    Router::new()
        .route("/", get(dashboard))
        .route("/static/style.css", get(stylesheet))
        .with_state(store)
}

async fn dashboard(State(store): State<Arc<MediaStore>>) -> Html<String> {
    // Best-effort per panel: a failing query renders an empty panel rather than
    // a 500, so a transient DB hiccup never blanks the whole dashboard.
    let counts = store.fetch_state_counts().await.unwrap_or_default();
    let saved = store.fetch_total_space_saved_bytes().await.unwrap_or(0);
    let backlog = store.fetch_backlog().await.unwrap_or_default();
    let breakdown = store
        .fetch_codec_state_breakdown()
        .await
        .unwrap_or_default();
    let failures = store.fetch_failure_breakdown().await.unwrap_or_default();
    let vmaf = store.fetch_vmaf_distribution().await.unwrap_or_default();
    let last_failure = store.fetch_last_failure().await.ok().flatten();

    Html(
        views::dashboard(
            &counts,
            saved,
            &backlog,
            &breakdown,
            &failures,
            &vmaf,
            last_failure.as_ref(),
        )
        .into_string(),
    )
}

async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("style.css"),
    )
}
