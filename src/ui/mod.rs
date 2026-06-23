use std::sync::Arc;

use axum::{
    Form, Router,
    extract::{Query, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Redirect},
    routing::{get, post},
};
use serde::Deserialize;
use tracing::{info, warn};

use crate::models::RetentionFileId;
use crate::store::{FailureClass, MediaStore};
use crate::transcode::reaper::reap_retention;

mod views;

/// State shared by the UI handlers: the store, plus an optional control token.
/// When the token is present the dashboard renders write actions (each form
/// carrying the token as a hidden field) and the `/actions/*` routes are
/// mounted; when absent the UI is strictly read-only.
#[derive(Clone)]
struct UiState {
    store: Arc<MediaStore>,
    control_token: Option<Arc<String>>,
    /// Quality bar for "confirmed" transcodes (`MIN_VMAF`): an original is only
    /// offered for deletion when its encode scored at or above this.
    min_vmaf: f64,
}

/// Operator dashboard, served from the same HTTP listener as the sync webhook.
/// Reads straight through the existing `MediaStore` pool — no separate read-only
/// role — and renders server-side (maud), so there is no JS build and the whole
/// UI ships inside the binary. Auth is delegated to the reverse proxy in front
/// of the loopback bind; `control_token` gates the mutating actions on top of
/// that (and acts as a same-origin/CSRF guard since a cross-origin page cannot
/// read it back to forge a POST).
pub fn router(store: Arc<MediaStore>, control_token: Option<String>, min_vmaf: f64) -> Router {
    let state = UiState {
        store,
        control_token: control_token.map(Arc::new),
        min_vmaf,
    };

    let mut router = Router::new()
        .route("/", get(dashboard))
        .route("/static/style.css", get(stylesheet));

    if state.control_token.is_some() {
        router = router
            .route("/actions/requeue-failed", post(requeue_failed))
            .route(
                "/actions/delete-confirmed-originals",
                post(delete_confirmed_originals),
            );
        info!("UI write actions enabled (UI_CONTROL_TOKEN set)");
    } else {
        info!("UI read-only (UI_CONTROL_TOKEN unset)");
    }

    router.with_state(state)
}

/// Flash carried across the POST→redirect→GET, set by an action handler and
/// rendered as a banner. At most one field is set per redirect.
#[derive(Debug, Deserialize)]
struct DashboardQuery {
    /// Number of failed files requeued.
    requeued: Option<i64>,
    /// Number of confirmed originals deleted.
    deleted: Option<i64>,
    /// GB freed by the deletion, paired with `deleted`.
    freed_gb: Option<f64>,
}

async fn dashboard(
    State(state): State<UiState>,
    Query(query): Query<DashboardQuery>,
) -> Html<String> {
    let store = &state.store;
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
    let retention = store
        .fetch_retention_summary(state.min_vmaf)
        .await
        .unwrap_or_default();
    let vmaf = store.fetch_vmaf_distribution().await.unwrap_or_default();
    let last_failure = store.fetch_last_failure().await.ok().flatten();

    // Token is embedded server-side into action forms only when control is on.
    let control = state.control_token.as_deref().map(String::as_str);

    Html(
        views::dashboard(views::DashboardData {
            counts: &counts,
            saved_bytes: saved,
            backlog: &backlog,
            breakdown: &breakdown,
            failures: &failures,
            retention: &retention,
            min_vmaf: state.min_vmaf,
            vmaf: &vmaf,
            last_failure: last_failure.as_ref(),
            control,
            flash_requeued: query.requeued,
            flash_deleted: query.deleted.zip(query.freed_gb),
        })
        .into_string(),
    )
}

/// Form body of the failure requeue: the shared token plus the failure class to
/// requeue (its [`FailureClass::key`]).
#[derive(Debug, Deserialize)]
struct RequeueForm {
    token: String,
    class: String,
}

/// Requeue all currently-`failed` files of the posted class back to
/// `discovered`. Verifies the control token, then delegates to the store (no
/// raw SQL here); on success redirects back to `/` with a flash count.
async fn requeue_failed(
    State(state): State<UiState>,
    Form(form): Form<RequeueForm>,
) -> Result<Redirect, StatusCode> {
    // Routes are only mounted when the token is set, so this is always Some here.
    let expected = state
        .control_token
        .as_deref()
        .ok_or(StatusCode::NOT_FOUND)?;
    if form.token != **expected {
        warn!("requeue-failed rejected: bad control token");
        return Err(StatusCode::FORBIDDEN);
    }

    let class = FailureClass::from_key(&form.class).ok_or(StatusCode::BAD_REQUEST)?;

    let moved = state.store.requeue_failed(class).await.map_err(|e| {
        warn!(%e, "requeue-failed failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    info!(
        class = class.key(),
        n = moved.len(),
        "requeued failed files"
    );

    Ok(Redirect::to(&format!("/?requeued={}", moved.len())))
}

/// Token-only form body for actions that take no further parameters.
#[derive(Debug, Deserialize)]
struct TokenForm {
    token: String,
}

/// Delete the originals of quality-confirmed transcodes (`done` + VMAF ≥
/// `MIN_VMAF`) from retention, reclaiming their disk space. Verifies the token,
/// then delegates to the shared reaper; on success redirects with a flash of the
/// count and GB freed.
async fn delete_confirmed_originals(
    State(state): State<UiState>,
    Form(form): Form<TokenForm>,
) -> Result<Redirect, StatusCode> {
    let expected = state
        .control_token
        .as_deref()
        .ok_or(StatusCode::NOT_FOUND)?;
    if form.token != **expected {
        warn!("delete-confirmed-originals rejected: bad control token");
        return Err(StatusCode::FORBIDDEN);
    }

    let confirmed = state
        .store
        .fetch_confirmed_originals(state.min_vmaf)
        .await
        .map_err(|e| {
            warn!(%e, "delete-confirmed-originals: fetch failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let freed_bytes: i64 = confirmed.iter().map(|(_, _, size)| *size).sum();
    let files: Vec<(RetentionFileId, String)> = confirmed
        .into_iter()
        .map(|(id, path, _)| (id, path))
        .collect();

    let deleted = reap_retention(&state.store, &files).await;
    let freed_gb = freed_bytes as f64 / 1_000_000_000.0;
    info!(deleted, freed_gb, "deleted confirmed originals");

    Ok(Redirect::to(&format!(
        "/?deleted={deleted}&freed_gb={freed_gb:.1}"
    )))
}

async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("style.css"),
    )
}
