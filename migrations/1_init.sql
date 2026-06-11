-- ============================================================================
-- ENUMS
-- ============================================================================

CREATE TYPE workflow_state AS ENUM (
    'discovered',
    'probed',
    'analyzed',
    'pending_approval',
    'transcoding',
    'done',
    'skipped',
    'failed'
);

CREATE TYPE file_status AS ENUM (
    'present',
    'missing',
    'deleted'
);

-- ============================================================================
-- TRIGGER FUNCTIONS
-- ============================================================================

CREATE OR REPLACE FUNCTION update_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION notify_event_inserted()
RETURNS TRIGGER AS $$
BEGIN
    PERFORM pg_notify(
        'media_event',
        json_build_object(
            'event_id',      NEW.id,
            'media_file_id', NEW.media_file_id,
            'event_type',    NEW.event->>'type'
        )::text
    );
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- ============================================================================
-- CONTENT TABLES
-- ============================================================================

CREATE TABLE series (
    id         UUID         PRIMARY KEY,
    title      TEXT         NOT NULL,
    tmdb_id    INTEGER,
    rating     REAL,
    created_at TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX uq_series_tmdb
    ON series (tmdb_id) WHERE tmdb_id IS NOT NULL;
CREATE TRIGGER trg_series_updated_at
    BEFORE UPDATE ON series
    FOR EACH ROW EXECUTE FUNCTION update_updated_at();

CREATE TABLE movies (
    id         UUID         PRIMARY KEY,
    title      TEXT         NOT NULL,
    tmdb_id    INTEGER,
    rating     REAL,
    created_at TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX uq_movies_tmdb
    ON movies (tmdb_id) WHERE tmdb_id IS NOT NULL;
CREATE TRIGGER trg_movies_updated_at
    BEFORE UPDATE ON movies
    FOR EACH ROW EXECUTE FUNCTION update_updated_at();

CREATE TABLE episodes (
    id              UUID         PRIMARY KEY,
    series_id       UUID         NOT NULL REFERENCES series(id) ON DELETE CASCADE,
    season_number   SMALLINT     NOT NULL,
    episode_number  SMALLINT     NOT NULL,
    title           TEXT         NOT NULL,
    tmdb_id         INTEGER,
    rating          REAL,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX uq_episode_position
    ON episodes (series_id, season_number, episode_number);
CREATE INDEX idx_episodes_series_id ON episodes (series_id);
CREATE INDEX idx_episodes_tmdb_id
    ON episodes (tmdb_id) WHERE tmdb_id IS NOT NULL;
CREATE TRIGGER trg_episodes_updated_at
    BEFORE UPDATE ON episodes
    FOR EACH ROW EXECUTE FUNCTION update_updated_at();

-- ============================================================================
-- PROVIDER MAPPING TABLES
-- ============================================================================

CREATE TABLE series_provider_refs (
    series_id    UUID         NOT NULL REFERENCES series(id) ON DELETE CASCADE,
    provider     TEXT         NOT NULL,
    external_id  TEXT         NOT NULL,
    created_at   TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    PRIMARY KEY (provider, external_id)
);
CREATE INDEX idx_series_refs_lookup ON series_provider_refs (series_id);

CREATE TABLE movie_provider_refs (
    movie_id     UUID         NOT NULL REFERENCES movies(id) ON DELETE CASCADE,
    provider     TEXT         NOT NULL,
    external_id  TEXT         NOT NULL,
    created_at   TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    PRIMARY KEY (provider, external_id)
);
CREATE INDEX idx_movie_refs_lookup ON movie_provider_refs (movie_id);

-- ============================================================================
-- MEDIA FILES (artefacts physiques)
-- ============================================================================

CREATE TABLE media_files (
    id              UUID            PRIMARY KEY,

    -- Exactement un des deux: episode_id XOR movie_id
    episode_id      UUID            REFERENCES episodes(id) ON DELETE CASCADE,
    movie_id        UUID            REFERENCES movies(id)   ON DELETE CASCADE,

    -- Métadonnées filesystem
    file_path       TEXT            NOT NULL UNIQUE,
    size_bytes      BIGINT,

    -- Données ffprobe (NULL jusqu'à l'analyse)
    video_codec     TEXT,
    height          INTEGER,
    width           INTEGER,
    bitrate_kbps    INTEGER,
    framerate       TEXT,
    duration_seconds DOUBLE PRECISION,

    -- État du workflow + décision figée
    workflow_state  workflow_state  NOT NULL DEFAULT 'discovered',
    transcode_spec  JSONB,

    -- Présence physique du fichier (orthogonal au workflow)
    file_status     file_status     NOT NULL DEFAULT 'present',
    last_seen_at    TIMESTAMPTZ     NOT NULL DEFAULT NOW(),

    -- Mapping provider direct (1 fichier = 1 item Jellyfin, donc colonne OK)
    jellyfin_id     TEXT,

    created_at      TIMESTAMPTZ     NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ     NOT NULL DEFAULT NOW(),

    CONSTRAINT exactly_one_parent CHECK (
        (episode_id IS NOT NULL AND movie_id IS NULL) OR
        (episode_id IS NULL     AND movie_id IS NOT NULL)
    )
);

CREATE UNIQUE INDEX uq_media_files_jellyfin
    ON media_files (jellyfin_id) WHERE jellyfin_id IS NOT NULL;

CREATE INDEX idx_media_files_episode
    ON media_files (episode_id) WHERE episode_id IS NOT NULL;
CREATE INDEX idx_media_files_movie
    ON media_files (movie_id) WHERE movie_id IS NOT NULL;

-- Index partiel pour les requêtes de recovery au démarrage
CREATE INDEX idx_media_files_active
    ON media_files (workflow_state)
    WHERE workflow_state NOT IN ('done', 'skipped', 'failed');

-- Index partiel pour la détection des orphelins
CREATE INDEX idx_media_files_not_present
    ON media_files (file_status, last_seen_at)
    WHERE file_status <> 'present';

CREATE TRIGGER trg_media_files_updated_at
    BEFORE UPDATE ON media_files
    FOR EACH ROW EXECUTE FUNCTION update_updated_at();

-- ============================================================================
-- EVENTS (historique + déclencheur NOTIFY)
-- ============================================================================

CREATE TABLE events (
    id             BIGSERIAL    PRIMARY KEY,
    media_file_id  UUID         NOT NULL REFERENCES media_files(id) ON DELETE CASCADE,
    event          JSONB        NOT NULL,
    created_at     TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_events_media_file
    ON events (media_file_id, created_at DESC);
CREATE INDEX idx_events_type
    ON events ((event->>'type'), created_at DESC);

CREATE TRIGGER trg_events_notify
    AFTER INSERT ON events
    FOR EACH ROW EXECUTE FUNCTION notify_event_inserted();
