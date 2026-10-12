-- 0028: per-user watch-history recording toggle.
-- TRUE (default) records progress into watch_history as before; FALSE pauses
-- recording: progress writes stop persisting history rows and heuristic
-- ingest, while existing rows are left untouched.
ALTER TABLE privacy_settings
    ADD COLUMN IF NOT EXISTS record_history BOOLEAN NOT NULL DEFAULT TRUE;
