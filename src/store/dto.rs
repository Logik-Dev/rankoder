use uuid::Uuid;

use crate::models::workflow::WorkflowStateTag;

#[derive(sqlx::FromRow)]
pub(super) struct MediaFileRow {
    pub id: Uuid,
    pub workflow_state: WorkflowStateTag,
    pub episode_id: Option<Uuid>,
    pub movie_id: Option<Uuid>,
    pub file_path: String,
    pub size_bytes: Option<i64>,
    pub video_codec: Option<String>,
    pub height: Option<i32>,
    pub width: Option<i32>,
    pub bitrate_kbps: Option<i32>,
    pub framerate: Option<String>,
}
