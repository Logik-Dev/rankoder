CREATE TABLE retention_files (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    media_file_id       UUID NOT NULL REFERENCES media_files(id) ON DELETE CASCADE,
    retained_path       TEXT NOT NULL,
    original_size_bytes BIGINT NOT NULL,
    moved_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_retention_files_moved_at ON retention_files (moved_at);
