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

use crate::approval::ApprovalOrchestrator;
use crate::models::RetentionFileId;
use crate::notification::ApprovalResponse;
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
    /// Handle for applying approval decisions, sharing the MQTT listener's
    /// chokepoint. Present alongside `control_token` (same gate); the
    /// approve/reject routes and the pending-approval action forms exist only
    /// when it is set.
    approval: Option<Arc<ApprovalOrchestrator>>,
}

/// Operator dashboard, served from the same HTTP listener as the sync webhook.
/// Reads straight through the existing `MediaStore` pool — no separate read-only
/// role — and renders server-side (maud), so there is no JS build and the whole
/// UI ships inside the binary. Auth is delegated to the reverse proxy in front
/// of the loopback bind; `control_token` gates the mutating actions on top of
/// that (and acts as a same-origin/CSRF guard since a cross-origin page cannot
/// read it back to forge a POST).
pub fn router(
    store: Arc<MediaStore>,
    control_token: Option<String>,
    min_vmaf: f64,
    approval: Option<Arc<ApprovalOrchestrator>>,
) -> Router {
    let state = UiState {
        store,
        control_token: control_token.map(Arc::new),
        min_vmaf,
        approval,
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
            )
            .route("/actions/approve-batch", post(approve_batch))
            .route("/actions/reject-batch", post(reject_batch));
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
    /// Title of the batch just approved (→ transcoding).
    approved: Option<String>,
    /// Title of the batch just rejected (→ skipped).
    rejected: Option<String>,
}

/// Percent-encode a flash title for the redirect query string, without pulling
/// in a urlencoding crate: only the characters that would break a
/// `?key=value` pair (or HTML/URL parsing) are escaped, the rest pass through.
fn flash_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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
    let pending = store.fetch_pending_batches().await.unwrap_or_default();

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
            pending: &pending,
            control,
            flash_requeued: query.requeued,
            flash_deleted: query.deleted.zip(query.freed_gb),
            flash_approved: query.approved.as_deref(),
            flash_rejected: query.rejected.as_deref(),
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

/// Form body of an approve/reject action: the shared token plus the encoded
/// [`crate::models::batch::BatchKey`] of the batch to decide on.
#[derive(Debug, Deserialize)]
struct BatchDecisionForm {
    token: String,
    batch_id: String,
}

/// Approve a pending batch from the dashboard: move it `pending_approval →
/// transcoding`. Delegates to the shared [`ApprovalOrchestrator`] chokepoint, so
/// it is indistinguishable from an MQTT approval (and racing one is a safe
/// no-op).
async fn approve_batch(
    State(state): State<UiState>,
    Form(form): Form<BatchDecisionForm>,
) -> Result<Redirect, StatusCode> {
    apply_batch_decision(&state, form, true).await
}

/// Reject a pending batch from the dashboard: move it `pending_approval →
/// skipped`. Same chokepoint as [`approve_batch`].
async fn reject_batch(
    State(state): State<UiState>,
    Form(form): Form<BatchDecisionForm>,
) -> Result<Redirect, StatusCode> {
    apply_batch_decision(&state, form, false).await
}

/// Shared body of approve/reject: verify the control token, resolve the batch
/// title for the flash (best-effort, before it transitions out of pending),
/// then funnel the decision through the approval orchestrator. Redirects back to
/// `/` with an `approved`/`rejected` flash carrying the title.
async fn apply_batch_decision(
    state: &UiState,
    form: BatchDecisionForm,
    approved: bool,
) -> Result<Redirect, StatusCode> {
    let expected = state
        .control_token
        .as_deref()
        .ok_or(StatusCode::NOT_FOUND)?;
    if form.token != **expected {
        warn!("batch decision rejected: bad control token");
        return Err(StatusCode::FORBIDDEN);
    }

    // Set in lockstep with the control token, so this is always Some here.
    let approval = state.approval.as_ref().ok_or(StatusCode::NOT_FOUND)?;

    // Resolve a human title for the flash while the batch is still pending; fall
    // back to the raw id if the lookup fails (e.g. already decided).
    let title = match crate::models::batch::BatchKey::decode(&form.batch_id) {
        Ok(key) => state
            .store
            .fetch_batch_request_info(&key)
            .await
            .ok()
            .map(|i| i.title)
            .unwrap_or_else(|| form.batch_id.clone()),
        Err(_) => form.batch_id.clone(),
    };

    approval
        .apply_response(ApprovalResponse {
            batch_id: form.batch_id.clone(),
            approved,
        })
        .await
        .map_err(|e| {
            warn!(%e, batch_id = %form.batch_id, "batch decision failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    info!(batch_id = %form.batch_id, approved, "applied batch decision from UI");

    let param = if approved { "approved" } else { "rejected" };
    Ok(Redirect::to(&format!("/?{param}={}", flash_encode(&title))))
}

async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("style.css"),
    )
}
