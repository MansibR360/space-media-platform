-- Allow runtime HTTP source plugins (kind='source').
--
-- The aggregator, health prober and sync pipeline already consume
-- kind='source' plugin rows (fan-out search via {base}/search, resolution via
-- {base}/details → streams), and register-url tries to insert them — but the
-- original CHECK never admitted the value, so those inserts failed. Widen it.

ALTER TABLE plugins DROP CONSTRAINT IF EXISTS plugins_kind_check;

ALTER TABLE plugins ADD CONSTRAINT plugins_kind_check
    CHECK (kind IN ('metadata', 'streaming', 'source'));
