use std::fmt;

use uuid::Uuid;

use crate::models::{movie::MovieId, series::SeriesId};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BatchKey {
    Season { series_id: SeriesId, season: i16 },
    Movie { movie_id: MovieId },
}

impl fmt::Display for BatchKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BatchKey::Season { series_id, season } => {
                write!(f, "series:{}:s{}", series_id.as_uuid(), season)
            }
            BatchKey::Movie { movie_id } => {
                write!(f, "movie:{}", movie_id.as_uuid())
            }
        }
    }
}

impl BatchKey {
    pub fn encode(&self) -> String {
        self.to_string()
    }

    pub fn decode(s: &str) -> Result<Self, BatchKeyError> {
        let parts: Vec<&str> = s.split(':').collect();
        match parts.as_slice() {
            ["series", uuid_str, season_str] => {
                let uuid: Uuid = uuid_str.parse()?;
                let series_id = SeriesId::from(uuid);
                let season = season_str
                    .trim_start_matches('s')
                    .parse()
                    .map_err(BatchKeyError::InvalidSeason)?;
                Ok(BatchKey::Season { series_id, season })
            }
            ["movie", uuid_str] => {
                let uuid: Uuid = uuid_str.parse()?;
                let movie_id = MovieId::from(uuid);
                Ok(BatchKey::Movie { movie_id })
            }
            _ => Err(BatchKeyError::InvalidFormat(s.to_string())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BatchKeyError {
    #[error("invalid batch key format: {0}")]
    InvalidFormat(String),
    #[error("invalid uuid in batch key: {0}")]
    InvalidUuid(#[from] uuid::Error),
    #[error("invalid season number in batch key: {0}")]
    InvalidSeason(#[from] std::num::ParseIntError),
}
