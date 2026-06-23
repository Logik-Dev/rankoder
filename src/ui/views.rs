use maud::{DOCTYPE, Markup, html};

use crate::{
    models::workflow::WorkflowStateTag,
    store::{Backlog, CodecStateBreakdown, FailureBreakdownRow, FailureRecord},
};

const BYTES_PER_GB: f64 = 1_000_000_000.0;

/// The dashboard page: per-state counts, total space saved, the outstanding
/// backlog, the codec×state breakdown, failures grouped by cause, the VMAF
/// distribution and the most recent failure. Server-rendered, zero JS — a
/// `<meta refresh>` keeps it live-ish without a build step.
pub fn dashboard(
    counts: &[(WorkflowStateTag, i64)],
    saved_bytes: i64,
    backlog: &Backlog,
    breakdown: &[CodecStateBreakdown],
    failures: &[FailureBreakdownRow],
    vmaf: &[(i32, i64)],
    last_failure: Option<&FailureRecord>,
) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { "rankoder" }
                // Poor-man's live refresh: no JS, no websocket, no build.
                meta http-equiv="refresh" content="60";
                link rel="stylesheet" href="/static/style.css";
            }
            body {
                header {
                    h1 { "rankoder" }
                    span.version { "v" (env!("CARGO_PKG_VERSION")) }
                }

                section.tiles {
                    @for state in ALL_STATES {
                        (tile(label_for(*state), count_for(counts, *state)))
                    }
                    div.tile.saved {
                        span.n { (format!("{:.1}", saved_bytes as f64 / BYTES_PER_GB)) }
                        span.unit { "GB" }
                        span.label { "space saved" }
                    }
                }

                (backlog_panel(backlog))

                section {
                    h2 { "By codec & state" }
                    @if breakdown.is_empty() {
                        p.empty { "Nothing ingested yet." }
                    } @else {
                        (breakdown_table(breakdown))
                    }
                }

                section {
                    h2 { "VMAF distribution" }
                    @if vmaf.is_empty() {
                        p.empty { "No measured scores yet." }
                    } @else {
                        (vmaf_histogram(vmaf))
                    }
                }

                section {
                    h2 { "Failures by cause" }
                    @if failures.is_empty() {
                        p.empty { "No failed files." }
                    } @else {
                        (failure_breakdown_table(failures))
                    }
                }

                section.failure {
                    h2 { "Last failure" }
                    @match last_failure {
                        Some(f) => {
                            p {
                                span.kind { (f.kind) }
                                " "
                                span.title { (f.title.as_deref().unwrap_or("(untitled)")) }
                            }
                            p.reason { (f.error) }
                        }
                        None => p.empty { "None." }
                    }
                }
            }
        }
    }
}

/// Workflow states in pipeline order, so the tiles always read left-to-right
/// the way the pipeline flows (and missing states render as zero, not absent).
const ALL_STATES: &[WorkflowStateTag] = &[
    WorkflowStateTag::Discovered,
    WorkflowStateTag::Probed,
    WorkflowStateTag::Analyzed,
    WorkflowStateTag::PendingApproval,
    WorkflowStateTag::Transcoding,
    WorkflowStateTag::Done,
    WorkflowStateTag::Skipped,
    WorkflowStateTag::Failed,
];

fn label_for(state: WorkflowStateTag) -> &'static str {
    match state {
        WorkflowStateTag::Discovered => "discovered",
        WorkflowStateTag::Probed => "probed",
        WorkflowStateTag::Analyzed => "analyzed",
        WorkflowStateTag::PendingApproval => "pending",
        WorkflowStateTag::Transcoding => "transcoding",
        WorkflowStateTag::Done => "done",
        WorkflowStateTag::Skipped => "skipped",
        WorkflowStateTag::Failed => "failed",
    }
}

fn count_for(counts: &[(WorkflowStateTag, i64)], state: WorkflowStateTag) -> i64 {
    counts
        .iter()
        .find(|(s, _)| *s == state)
        .map(|(_, c)| *c)
        .unwrap_or(0)
}

fn tile(label: &str, count: i64) -> Markup {
    html! {
        div.tile {
            span.n { (count) }
            span.label { (label) }
        }
    }
}

/// The outstanding work: files decided but not yet transcoded, framed so the
/// "space saved" tile reads against what is still to gain.
fn backlog_panel(b: &Backlog) -> Markup {
    html! {
        section {
            h2 { "Backlog (analyzed → transcoding)" }
            div.tiles {
                (tile_unit(&b.file_count.to_string(), "", "files queued"))
                (tile_unit(&format!("{:.1}", b.total_bytes as f64 / BYTES_PER_GB), "GB", "queued size"))
                div.tile.projected {
                    span.n { (format!("{:.1}", b.projected_saved_bytes as f64 / BYTES_PER_GB)) }
                    span.unit { "GB" }
                    span.label { "projected savings" }
                }
            }
        }
    }
}

/// A tile with an explicit unit + label (parallel to the inline `.saved` tile).
fn tile_unit(n: &str, unit: &str, label: &str) -> Markup {
    html! {
        div.tile {
            span.n { (n) }
            @if !unit.is_empty() { span.unit { (unit) } }
            span.label { (label) }
        }
    }
}

/// Rows of `codec · state · count · size`, ordered as the store returns them
/// (codec, then descending count). Size is shown in GB to one decimal.
fn breakdown_table(rows: &[CodecStateBreakdown]) -> Markup {
    html! {
        table.breakdown {
            thead {
                tr { th { "codec" } th { "state" } th.num { "files" } th.num { "size (GB)" } }
            }
            tbody {
                @for r in rows {
                    tr {
                        td { (r.codec) }
                        td { (label_for(r.state)) }
                        td.num { (r.count) }
                        td.num { (format!("{:.1}", r.total_bytes as f64 / BYTES_PER_GB)) }
                    }
                }
            }
        }
    }
}

/// Failure causes with counts. The hint column distinguishes classes a requeue
/// can fix on its own from environmental ones that need a host/source fix first
/// — so the operator doesn't burn encodes re-driving doomed files.
fn failure_breakdown_table(rows: &[FailureBreakdownRow]) -> Markup {
    html! {
        table.breakdown {
            thead {
                tr { th { "cause" } th.num { "files" } th { "remediation" } }
            }
            tbody {
                @for r in rows {
                    tr {
                        td { (r.class.label()) }
                        td.num { (r.count) }
                        @if r.class.auto_requeueable() {
                            td.hint.ok { "requeue safe" }
                        } @else {
                            td.hint.warn { "needs host/source fix first" }
                        }
                    }
                }
            }
        }
    }
}

fn vmaf_histogram(buckets: &[(i32, i64)]) -> Markup {
    let max = buckets.iter().map(|(_, c)| *c).max().unwrap_or(1).max(1);
    html! {
        div.histogram {
            @for (score, count) in buckets {
                div.bar
                    style=(format!("height:{}%", count * 100 / max))
                    title=(format!("VMAF {score}: {count}")) {
                        span.tick { (score) }
                    }
            }
        }
    }
}
