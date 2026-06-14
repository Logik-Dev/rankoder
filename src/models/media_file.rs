use std::str::FromStr;

use uuid::Uuid;

use crate::{
    impl_entity_id,
    models::{
        common::AbsoluteFilePath, episode::EpisodeId, error::DomainError, movie::MovieId,
        video::VideoProperties, workflow::WorkflowStateTag,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct MediaFileId(Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SizeBytes(u64);

impl SizeBytes {
    pub fn new(value: u64) -> Result<Self, DomainError> {
        if value == 0 {
            return Err(DomainError::InvalidSizeBytes);
        }

        Ok(Self(value))
    }

    pub fn as_gb(&self) -> f64 {
        self.as_u64() as f64 / 1024.0 / 1024.0 / 1024.0
    }

    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl FromStr for SizeBytes {
    type Err = DomainError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let value = s
            .parse::<u64>()
            .map_err(|_| DomainError::InvalidSizeBytes)?;

        Self::new(value)
    }
}

pub struct MediaFile {
    pub id: MediaFileId,
    pub episode_id: Option<EpisodeId>,
    pub movie_id: Option<MovieId>,
    pub path: AbsoluteFilePath,
    pub video_properties: Option<VideoProperties>,
    pub transcode_spec: Option<serde_json::Value>,
    pub workflow_state: WorkflowStateTag,
}

impl_entity_id!(MediaFileId);
