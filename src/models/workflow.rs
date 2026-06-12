use crate::models::event::MediaEvent;

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

impl WorkflowStateTag {
    pub fn next_on(self, event: &MediaEvent) -> Option<WorkflowStateTag> {
        use MediaEvent as E;
        use WorkflowStateTag as S;
        Some(match (self, event) {
            (S::Discovered, E::Probed) => S::Probed,
            (S::Discovered, E::ProbeFailed { .. }) => S::Failed,
            (S::Probed, E::Analyzed { .. }) => S::Analyzed,
            (S::Probed, E::Skipped { .. }) => S::Skipped,
            (S::Analyzed, E::PendingApproval) => S::PendingApproval,
            (S::PendingApproval, E::ApprovalGranted) => S::Transcoding,
            (S::PendingApproval, E::ApprovalRejected) => S::Skipped,
            _ => return None,
        })
    }
}
