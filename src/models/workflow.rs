#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "workflow_state", rename_all = "snake_case")]
pub enum WorkflowStateTag {
    Discovered,
    Probed,
    Analyzed,
    PendingApproval,
    Transcoding,
    Done,
    Skipped,
    Failed,
}
