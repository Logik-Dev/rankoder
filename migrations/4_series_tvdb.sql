-- Sonarr indexes series by TheTVDB id, not TMDB. Capture it (Jellyfin already
-- provides it in ProviderIds) so we can ask Sonarr to rescan after transcoding.
ALTER TABLE series ADD COLUMN tvdb_id INTEGER;

CREATE UNIQUE INDEX uq_series_tvdb
    ON series (tvdb_id) WHERE tvdb_id IS NOT NULL;
