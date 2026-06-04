CREATE TABLE series (
    id UUID PRIMARY KEY,
    title TEXT NOT NULL,
    tmdb_id INTEGER,
    rating REAL,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_series_tmdb_id ON series(tmdb_id);

CREATE TABLE episodes (
    id UUID PRIMARY KEY,
    series_id UUID NOT NULL REFERENCES series(id),
    season_number INTEGER,
    episode_number INTEGER,
    title TEXT NOT NULL,
    tmdb_id INTEGER,
    rating REAL,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_episodes_series_id ON episodes(series_id);
CREATE INDEX idx_episodes_tmdb_id ON episodes(tmdb_id);

CREATE TABLE movies (
    id UUID PRIMARY KEY,
    title TEXT NOT NULL,
    tmdb_id INTEGER,
    rating REAL,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_movies_tmdb_id ON movies(tmdb_id);

CREATE TABLE media_files (
    id           UUID         NOT NULL PRIMARY KEY,
    episode_id   UUID         REFERENCES episodes(id),
    movie_id     UUID         REFERENCES movies(id),
    file_path    TEXT         NOT NULL UNIQUE,
    size_bytes   BIGINT,
    video_codec  TEXT,
    height       INTEGER,
    width        INTEGER,
    bitrate_kbps INTEGER,
    framerate    REAL,
    jellyfin_id  TEXT         UNIQUE,
    status       TEXT         NOT NULL DEFAULT 'present',
    created_at   TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    last_seen_at TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    CHECK (
        (episode_id IS NOT NULL AND movie_id IS NULL) OR
        (episode_id IS NULL AND movie_id IS NOT NULL)
    )
);
