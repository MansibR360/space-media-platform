CREATE TABLE library_paths (
    id UUID PRIMARY KEY,
    root_path TEXT NOT NULL UNIQUE,
    label TEXT,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE media_files (
    id UUID PRIMARY KEY,
    media_id UUID REFERENCES media (id) ON DELETE SET NULL,
    episode_id UUID REFERENCES episodes (id) ON DELETE SET NULL,
    library_path_id UUID REFERENCES library_paths (id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    file_name TEXT NOT NULL,
    file_size BIGINT NOT NULL DEFAULT 0,
    modified_at TIMESTAMPTZ,
    container TEXT,
    duration_seconds DOUBLE PRECISION,
    width INTEGER,
    height INTEGER,
    video_codec TEXT,
    audio_codec TEXT,
    bitrate INTEGER,
    probed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (library_path_id, relative_path)
);

CREATE INDEX idx_media_files_media ON media_files (media_id);
CREATE INDEX idx_media_files_episode ON media_files (episode_id);

CREATE TABLE media_streams (
    id UUID PRIMARY KEY,
    file_id UUID NOT NULL REFERENCES media_files (id) ON DELETE CASCADE,
    stream_kind TEXT NOT NULL CHECK (stream_kind IN ('video', 'audio', 'subtitle')),
    stream_index INTEGER NOT NULL,
    codec TEXT,
    language TEXT,
    title TEXT,
    channels INTEGER,
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    is_forced BOOLEAN NOT NULL DEFAULT FALSE,
    external_path TEXT,
    UNIQUE (file_id, stream_index)
);

CREATE INDEX idx_media_streams_file ON media_streams (file_id);

CREATE TABLE playback_sessions (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    file_id UUID NOT NULL REFERENCES media_files (id) ON DELETE CASCADE,
    device_name TEXT NOT NULL DEFAULT '',
    client TEXT NOT NULL DEFAULT '',
    audio_stream_id UUID REFERENCES media_streams (id) ON DELETE SET NULL,
    subtitle_stream_id UUID REFERENCES media_streams (id) ON DELETE SET NULL,
    position_seconds DOUBLE PRECISION NOT NULL DEFAULT 0,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_active_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at TIMESTAMPTZ
);

CREATE INDEX idx_playback_sessions_user_active
    ON playback_sessions (user_id) WHERE ended_at IS NULL;
CREATE INDEX idx_playback_sessions_media_file ON playback_sessions (file_id);

CREATE TABLE transcode_jobs (
    id UUID PRIMARY KEY,
    file_id UUID NOT NULL REFERENCES media_files (id) ON DELETE CASCADE,
    job_kind TEXT NOT NULL CHECK (job_kind IN ('hls_transcode', 'thumbnails')),
    status TEXT NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued', 'running', 'completed', 'failed', 'cancelled')),
    progress SMALLINT NOT NULL DEFAULT 0,
    output_dir TEXT,
    error TEXT,
    created_by UUID REFERENCES users (id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ
);

CREATE INDEX idx_transcode_jobs_file ON transcode_jobs (file_id, status);

CREATE TABLE thumbnails (
    id UUID PRIMARY KEY,
    file_id UUID NOT NULL REFERENCES media_files (id) ON DELETE CASCADE,
    second_offset INTEGER NOT NULL,
    file_path TEXT NOT NULL,
    width INTEGER NOT NULL DEFAULT 320,
    height INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (file_id, second_offset)
);

CREATE INDEX idx_thumbnails_file ON thumbnails (file_id, second_offset);
