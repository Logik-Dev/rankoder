CREATE TABLE video_color_metadata (
    media_file_id   UUID PRIMARY KEY REFERENCES media_files(id) ON DELETE CASCADE,
    color_primaries TEXT,
    color_trc       TEXT,
    colorspace      TEXT,
    master_display  TEXT,
    max_cll         TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
