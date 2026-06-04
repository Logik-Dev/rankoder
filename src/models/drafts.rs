use crate::models::{
    common::{AbsoluteFilePath, EpisodeNumber, Rating, SeasonNumber, TmdbId},
    series::SeriesId,
};

#[derive(Debug)]
pub enum Provider {
    Jellyfin,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Jellyfin => "jellyfin",
        }
    }
}

#[derive(Debug)]
pub struct SeriesDraft {
    pub title: String,
    pub tmdb_id: Option<TmdbId>,
    pub rating: Option<Rating>,
    pub jellyfin_id: String,
    pub provider: Provider,
}

#[derive(Debug)]
pub struct MediaFileDraft {
    pub jellyfin_id: String,
    pub path: AbsoluteFilePath,
    pub size_bytes: Option<i64>,
}

#[derive(Debug)]
pub struct MovieDraft {
    pub title: String,
    pub tmdb_id: Option<TmdbId>,
    pub rating: Option<Rating>,
    // TODO make it generic
    pub jellyfin_id: String,
    pub media_file: MediaFileDraft,
    pub provider: Provider,
}

#[derive(Debug)]
pub struct EpisodeDraft {
    pub title: String,
    pub series_id: SeriesId,
    pub season_number: SeasonNumber,
    pub episode_number: EpisodeNumber,
    pub tmdb_id: Option<TmdbId>,
    pub rating: Option<Rating>,
    pub provider: Provider,
    pub media_file: MediaFileDraft,
}
