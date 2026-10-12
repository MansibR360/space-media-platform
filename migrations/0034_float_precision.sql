-- Widen every REAL column to DOUBLE PRECISION.
--
-- The Rust side decodes all of these as `f64`, but they were declared REAL
-- (FLOAT4). sqlx refuses that pairing outright — it does not silently widen —
-- so each mismatch is a hard runtime failure the moment the column is read:
--
--   error occurred while decoding column "challenge_threshold": mismatched
--   types; Rust type `f64` (as SQL type `FLOAT8`) is not compatible with
--   SQL type `FLOAT4`
--
-- That took down playback authorization (risk_policies), full-text search
-- (ts_rank, fixed in the query) and the ratings reload (source_ratings.score,
-- fixed in 0032). This migration removes the remaining instances so the class
-- of bug cannot recur from the schema side.

ALTER TABLE security_events    ALTER COLUMN score               TYPE DOUBLE PRECISION;
ALTER TABLE risk_policies      ALTER COLUMN challenge_threshold TYPE DOUBLE PRECISION;
ALTER TABLE risk_policies      ALTER COLUMN revoke_threshold    TYPE DOUBLE PRECISION;
ALTER TABLE media_relationships ALTER COLUMN confidence         TYPE DOUBLE PRECISION;
ALTER TABLE heuristic_events   ALTER COLUMN weight              TYPE DOUBLE PRECISION;
ALTER TABLE heuristic_events   ALTER COLUMN percent             TYPE DOUBLE PRECISION;
ALTER TABLE heuristic_events   ALTER COLUMN rating              TYPE DOUBLE PRECISION;
