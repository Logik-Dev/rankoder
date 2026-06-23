use maud::{DOCTYPE, Markup, html};

use crate::{
    models::workflow::WorkflowStateTag,
    store::{Backlog, CodecStateBreakdown, FailureBreakdownRow, FailureRecord},
};

const BYTES_PER_GB: f64 = 1_000_000_000.0;

/// Everything the dashboard page renders. Grouped into a struct because the
/// list of panels (and now the optional control token + flash) has outgrown a
/// readable positional argument list.
pub struct DashboardData<'a> {
    pub counts: &'a [(WorkflowStateTag, i64)],
    pub saved_bytes: i64,
    pub backlog: &'a Backlog,
    pub breakdown: &'a [CodecStateBreakdown],
    pub failures: &'a [FailureBreakdownRow],
    pub vmaf: &'a [(i32, i64)],
    pub last_failure: Option<&'a FailureRecord>,
    /// `Some(token)` when write actions are enabled: the dashboard then renders
    /// action forms with this token embedded as a hidden field. `None` keeps the
    /// page strictly read-only.
    pub control: Option<&'a str>,
    /// Flash: number of files requeued by the action that redirected here.
    pub flash_requeued: Option<i64>,
}

/// The dashboard page: per-state counts, total space saved, the outstanding
/// backlog, the codec×state breakdown, failures grouped by cause, the VMAF
/// distribution and the most recent failure. Server-rendered, zero JS — a
/// `<meta refresh>` keeps it live-ish without a build step.
pub fn dashboard(d: DashboardData<'_>) -> Markup {
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

                @if let Some(n) = d.flash_requeued {
                    p.flash { "Requeued " (n) " file" @if n != 1 { "s" } " to discovered." }
                }

                section.tiles {
                    @for state in ALL_STATES {
                        (tile(label_for(*state), count_for(d.counts, *state)))
                    }
                    div.tile.saved {
                        span.n { (format!("{:.1}", d.saved_bytes as f64 / BYTES_PER_GB)) }
                        span.unit { "GB" }
                        span.label { "space saved" }
                    }
                }

                (backlog_panel(d.backlog))

                section {
                    h2 { "By codec & state" }
                    @if d.breakdown.is_empty() {
                        p.empty { "Nothing ingested yet." }
                    } @else {
                        (breakdown_table(d.breakdown))
                    }
                }

                section {
                    h2 { "VMAF distribution" }
                    @if d.vmaf.is_empty() {
                        p.empty { "No measured scores yet." }
                    } @else {
                        (vmaf_histogram(d.vmaf))
                    }
                }

                section {
                    h2 { "Failures by cause" }
                    @if d.failures.is_empty() {
                        p.empty { "No failed files." }
                    } @else {
                        (failure_breakdown_table(d.failures, d.control))
                    }
                }

                section.failure {
                    h2 { "Last failure" }
                    @match d.last_failure {
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
/// — so the operator doesn't burn encodes re-driving doomed files. When
/// `control` is `Some(token)`, each row also gets a requeue form (the token goes
/// in as a hidden field); when `None` the table stays read-only.
fn failure_breakdown_table(rows: &[FailureBreakdownRow], control: Option<&str>) -> Markup {
    html! {
        table.breakdown {
            thead {
                tr {
                    th { "cause" }
                    th.num { "files" }
                    th { "remediation" }
                    @if control.is_some() { th { "action" } }
                }
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
                        @if let Some(token) = control {
                            td {
                                form method="post" action="/actions/requeue-failed" {
                                    input type="hidden" name="token" value=(token);
                                    input type="hidden" name="class" value=(r.class.key());
                                    button type="submit" { "Requeue" }
                                }
                            }
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
