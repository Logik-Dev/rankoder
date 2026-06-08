use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::models::error::DomainError;

#[derive(Debug, sqlx::Type)]
#[sqlx(transparent)]
pub struct TmdbId(i32);

impl TmdbId {
    pub fn new(value: i32) -> Result<Self, DomainError> {
        if value < 0 {
            return Err(DomainError::InvalidTmdbId(format!("{value}")));
        }

        Ok(Self(value))
    }
}

impl FromStr for TmdbId {
    type Err = DomainError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse()
            .map_err(|_| DomainError::InvalidTmdbId(s.to_string()))
            .and_then(Self::new)
    }
}

#[derive(Debug, sqlx::Type)]
#[sqlx(transparent)]
pub struct Rating(f32);

impl Rating {
    pub fn new(value: f32) -> Result<Self, DomainError> {
        if !(0.0..=10.0).contains(&value) {
            return Err(DomainError::InvalidRating(value));
        }

        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AbsoluteFilePath(PathBuf);

impl AbsoluteFilePath {
    pub fn new(path: impl AsRef<std::path::Path>) -> Result<Self, DomainError> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(DomainError::InvalidPath(
                path.to_str().unwrap_or("<invalid UTF-8>").to_string(),
            ));
        }
        Ok(Self(path.to_path_buf()))
    }
}

impl AsRef<Path> for AbsoluteFilePath {
    fn as_ref(&self) -> &Path {
        self.0.as_path()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct SeasonNumber(u32);

impl SeasonNumber {
    pub fn new(value: i32) -> Result<Self, DomainError> {
        if value < 0 {
            return Err(DomainError::InvalidSeasonNumber(value));
        }
        Ok(Self(value as u32))
    }

    pub fn as_i16(&self) -> i16 {
        self.0 as i16
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EpisodeNumber(u32);

impl EpisodeNumber {
    pub fn new(value: i32) -> Result<Self, DomainError> {
        if value < 0 {
            return Err(DomainError::InvalidEpisodeNumber(value));
        }
        Ok(Self(value as u32))
    }

    pub fn as_i16(&self) -> i16 {
        self.0 as i16
    }
}
