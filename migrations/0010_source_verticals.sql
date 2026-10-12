ALTER TABLE media DROP CONSTRAINT media_media_type_check;
ALTER TABLE media ADD CONSTRAINT media_media_type_check
    CHECK (media_type IN ('movie', 'series', 'book', 'music', 'manga'));

ALTER TABLE media ADD COLUMN IF NOT EXISTS artist TEXT;

CREATE INDEX IF NOT EXISTS idx_media_artist ON media (artist) WHERE media_type = 'music';
