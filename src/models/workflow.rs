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
            (S::Transcoding, E::Transcoded { .. }) => S::Done,
            (S::Transcoding, E::TranscodeFailed { .. }) => S::Failed,
            (S::Transcoding, E::Skipped { .. }) => S::Skipped,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{event::MediaEvent, transcode::SkipReason};

    #[test]
    fn transcoding_to_done_on_transcoded() {
        assert_eq!(
            WorkflowStateTag::Transcoding.next_on(&MediaEvent::Transcoded {
                original_size: 5000000,
                new_size: 2000000,
            }),
            Some(WorkflowStateTag::Done)
        );
    }

    #[test]
    fn transcoding_to_failed_on_transcode_failed() {
        assert_eq!(
            WorkflowStateTag::Transcoding.next_on(&MediaEvent::TranscodeFailed {
                error: "boom".into(),
            }),
            Some(WorkflowStateTag::Failed)
        );
    }

    #[test]
    fn transcoding_to_skipped_on_skipped() {
        assert_eq!(
            WorkflowStateTag::Transcoding.next_on(&MediaEvent::Skipped {
                reason: SkipReason::InsufficientSizeReduction,
                bpp: None,
                compression_potential: None,
            }),
            Some(WorkflowStateTag::Skipped)
        );
    }
}
