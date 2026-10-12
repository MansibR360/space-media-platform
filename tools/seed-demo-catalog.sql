-- Demo catalog for local UI work.
--
-- Covers every media_type the frontend renders — movie, series, book, manga,
-- music — plus genres, seasons/episodes, IMDb ratings and music analysis rows,
-- so each surface has something real to display.
--
-- Safe to re-run: every insert is keyed on a deterministic UUID and upserts.
-- Remove it all with the DELETE at the bottom of this file.

BEGIN;

-- ------------------------------------------------------------------ genres
INSERT INTO genres (id, name, slug) VALUES
  ('00000000-0000-4000-9000-000000000001', 'Science Fiction', 'science-fiction'),
  ('00000000-0000-4000-9000-000000000002', 'Drama',           'drama'),
  ('00000000-0000-4000-9000-000000000003', 'Thriller',        'thriller'),
  ('00000000-0000-4000-9000-000000000004', 'Fantasy',         'fantasy'),
  ('00000000-0000-4000-9000-000000000005', 'Electronic',      'electronic'),
  ('00000000-0000-4000-9000-000000000006', 'Jazz',            'jazz'),
  ('00000000-0000-4000-9000-000000000007', 'Adventure',       'adventure'),
  ('00000000-0000-4000-9000-000000000008', 'Ambient',         'ambient')
ON CONFLICT (id) DO NOTHING;

-- ------------------------------------------------------------------- media
INSERT INTO media (
    id, media_type, is_anime, title, overview, release_date, runtime_minutes,
    status, age_rating, popularity, rating_avg, rating_count,
    imdb_id, imdb_rating, imdb_votes, metascore, content_rating, imdb_synced_at
) VALUES
  -- movies
  ('10000000-0000-4000-9000-000000000001', 'movie', false, 'The Silent Orbit',
   'A salvage crew wakes to find their station has drifted a decade off course, and something aboard has been keeping the lights on.',
   '2023-09-14', 128, 'released', 'PG-13', 940, 4.4, 210,
   'tt0000001', 8.1, 245000, 74, 'PG-13', now()),
  ('10000000-0000-4000-9000-000000000002', 'movie', false, 'Paper Cities',
   'An architect returns to the town she designed and finds every building subtly wrong.',
   '2022-04-01', 104, 'released', 'R', 720, 4.1, 130,
   'tt0000002', 7.4, 88000, 68, 'R', now()),
  ('10000000-0000-4000-9000-000000000003', 'movie', false, 'Winterlight',
   'Two rival cartographers race to chart a mountain range that keeps rearranging itself.',
   '2024-11-22', 116, 'released', 'PG', 880, 4.6, 305,
   'tt0000003', 8.7, 412000, 88, 'PG', now()),
  ('10000000-0000-4000-9000-000000000004', 'movie', false, 'Low Tide',
   'A fishing village keeps a secret that surfaces only twice a year.',
   '2021-06-18', 97, 'released', 'PG-13', 410, 3.8, 74,
   'tt0000004', 6.9, 31000, 55, 'PG-13', now()),

  -- series
  ('10000000-0000-4000-9000-000000000010', 'series', false, 'The Long Signal',
   'A radio astronomer spends thirty years decoding eleven seconds of noise.',
   '2020-01-08', 52, 'ongoing', 'TV-14', 990, 4.7, 520,
   'tt0000010', 8.9, 190000, NULL, 'TV-14', now()),
  ('10000000-0000-4000-9000-000000000011', 'series', true, 'Kite Season',
   'Every summer the island children build kites; this year one of them comes back with a passenger.',
   '2022-07-03', 24, 'finished', 'TV-PG', 860, 4.5, 340,
   NULL, NULL, NULL, NULL, NULL, NULL),

  -- books
  ('10000000-0000-4000-9000-000000000020', 'book', false, 'The Cartographer''s Apology',
   'A posthumous collection of maps for places that were never built.',
   '2019-03-12', NULL, 'released', NULL, 300, 4.2, 62,
   NULL, NULL, NULL, NULL, NULL, NULL),
  ('10000000-0000-4000-9000-000000000021', 'book', false, 'Nine Ways to Fold Light',
   'Essays on optics, grief, and the physics of stained glass.',
   '2021-10-05', NULL, 'released', NULL, 260, 4.0, 41,
   NULL, NULL, NULL, NULL, NULL, NULL),

  -- manga
  ('10000000-0000-4000-9000-000000000030', 'manga', true, 'Tidewalker',
   'A boy who can breathe underwater apprentices to a lighthouse keeper who cannot swim.',
   '2018-05-20', NULL, 'ongoing', 'TV-PG', 700, 4.6, 188,
   NULL, NULL, NULL, NULL, NULL, NULL),
  ('10000000-0000-4000-9000-000000000031', 'manga', true, 'Salt & Ember',
   'Two blacksmiths on opposite sides of a war forge halves of the same blade.',
   '2020-02-14', NULL, 'ongoing', 'TV-14', 640, 4.3, 121,
   NULL, NULL, NULL, NULL, NULL, NULL),

  -- music
  ('10000000-0000-4000-9000-000000000040', 'music', false, 'Meridian Drift',
   'Opening track from the Longwave sessions.',
   '2023-02-10', NULL, 'released', NULL, 560, 4.3, 96,
   NULL, NULL, NULL, NULL, NULL, NULL),
  ('10000000-0000-4000-9000-000000000041', 'music', false, 'Harbour Lights',
   'Late-night piano and brushed drums.',
   '2022-08-19', NULL, 'released', NULL, 480, 4.1, 71,
   NULL, NULL, NULL, NULL, NULL, NULL),
  ('10000000-0000-4000-9000-000000000042', 'music', false, 'Static Bloom',
   'Four minutes of slowly resolving noise.',
   '2024-01-27', NULL, 'released', NULL, 620, 4.5, 143,
   NULL, NULL, NULL, NULL, NULL, NULL)
ON CONFLICT (id) DO UPDATE SET
  title = EXCLUDED.title,
  overview = EXCLUDED.overview,
  imdb_rating = EXCLUDED.imdb_rating,
  imdb_votes = EXCLUDED.imdb_votes,
  metascore = EXCLUDED.metascore,
  content_rating = EXCLUDED.content_rating;

-- ------------------------------------------------------------ media_genres
INSERT INTO media_genres (media_id, genre_id) VALUES
  ('10000000-0000-4000-9000-000000000001', '00000000-0000-4000-9000-000000000001'),
  ('10000000-0000-4000-9000-000000000001', '00000000-0000-4000-9000-000000000003'),
  ('10000000-0000-4000-9000-000000000002', '00000000-0000-4000-9000-000000000002'),
  ('10000000-0000-4000-9000-000000000003', '00000000-0000-4000-9000-000000000004'),
  ('10000000-0000-4000-9000-000000000003', '00000000-0000-4000-9000-000000000007'),
  ('10000000-0000-4000-9000-000000000004', '00000000-0000-4000-9000-000000000002'),
  ('10000000-0000-4000-9000-000000000010', '00000000-0000-4000-9000-000000000001'),
  ('10000000-0000-4000-9000-000000000010', '00000000-0000-4000-9000-000000000002'),
  ('10000000-0000-4000-9000-000000000011', '00000000-0000-4000-9000-000000000004'),
  ('10000000-0000-4000-9000-000000000020', '00000000-0000-4000-9000-000000000002'),
  ('10000000-0000-4000-9000-000000000021', '00000000-0000-4000-9000-000000000002'),
  ('10000000-0000-4000-9000-000000000030', '00000000-0000-4000-9000-000000000004'),
  ('10000000-0000-4000-9000-000000000031', '00000000-0000-4000-9000-000000000007'),
  ('10000000-0000-4000-9000-000000000040', '00000000-0000-4000-9000-000000000005'),
  ('10000000-0000-4000-9000-000000000041', '00000000-0000-4000-9000-000000000006'),
  ('10000000-0000-4000-9000-000000000042', '00000000-0000-4000-9000-000000000008')
ON CONFLICT DO NOTHING;

-- ---------------------------------------------------- seasons and episodes
INSERT INTO seasons (id, media_id, number, title, overview, air_date) VALUES
  ('20000000-0000-4000-9000-000000000001', '10000000-0000-4000-9000-000000000010', 1,
   'Carrier Wave', 'The first eleven seconds.', '2020-01-08'),
  ('20000000-0000-4000-9000-000000000002', '10000000-0000-4000-9000-000000000010', 2,
   'Sidebands', 'What the noise was hiding.', '2021-03-19')
ON CONFLICT (id) DO NOTHING;

INSERT INTO episodes (id, media_id, season_id, number, title, overview, air_date, runtime_minutes) VALUES
  ('30000000-0000-4000-9000-000000000001', '10000000-0000-4000-9000-000000000010',
   '20000000-0000-4000-9000-000000000001', 1, 'Eleven Seconds',
   'A night shift, a printout, and a number that should not repeat.', '2020-01-08', 54),
  ('30000000-0000-4000-9000-000000000002', '10000000-0000-4000-9000-000000000010',
   '20000000-0000-4000-9000-000000000001', 2, 'The Second Listen',
   'Nobody believes her, so she books the dish again.', '2020-01-15', 51),
  ('30000000-0000-4000-9000-000000000003', '10000000-0000-4000-9000-000000000010',
   '20000000-0000-4000-9000-000000000001', 3, 'Interference',
   'A grant runs out.', '2020-01-22', 49),
  ('30000000-0000-4000-9000-000000000004', '10000000-0000-4000-9000-000000000010',
   '20000000-0000-4000-9000-000000000002', 1, 'Thirty Years On',
   'The signal returns, and so does she.', '2021-03-19', 58)
ON CONFLICT (id) DO NOTHING;

-- ------------------------------------------ music analysis + global stats
-- Values in the range a real analyzer produces, so the music surfaces have
-- tempo, key and descriptors to render.
INSERT INTO music_analysis (
    media_id, duration_seconds, sample_rate, channels, bitrate_kbps, codec,
    artist, album, release_year, loudness_lufs, peak_db, dynamic_range, energy,
    tempo_bpm, tempo_confidence, musical_key, musical_mode, key_confidence,
    danceability, valence, acousticness, instrumentalness, brightness,
    analysis_version, analyzed_at
) VALUES
  ('10000000-0000-4000-9000-000000000040', 247.0, 44100, 2, 320, 'mp3',
   'Longwave', 'Meridian', 2023, -9.2, -0.7, 6.4, 0.95,
   124.0, 0.82, 7, 1, 0.71, 0.78, 0.62, 0.21, 0.88, 0.54,
   2, now()),
  ('10000000-0000-4000-9000-000000000041', 198.0, 44100, 2, 256, 'mp3',
   'Ilse Brandt Trio', 'Harbour', 2022, -14.8, -1.9, 11.2, 0.69,
   88.0, 0.64, 2, 0, 0.66, 0.41, 0.38, 0.79, 0.55, 0.37,
   2, now()),
  ('10000000-0000-4000-9000-000000000042', 264.0, 48000, 2, 320, 'flac',
   'Vellum', 'Static Bloom', 2024, -18.4, -3.2, 14.6, 0.53,
   67.0, 0.35, 9, 0, 0.42, 0.22, 0.19, 0.64, 0.93, 0.71,
   2, now())
ON CONFLICT (media_id) DO UPDATE SET
  artist = EXCLUDED.artist,
  tempo_bpm = EXCLUDED.tempo_bpm,
  musical_key = EXCLUDED.musical_key,
  analyzed_at = EXCLUDED.analyzed_at;

INSERT INTO music_track_stats (
    media_id, plays, completions, skips, unique_listeners, seconds_played,
    avg_percent, completion_rate, skip_rate, quality, trend,
    first_played_at, last_played_at
) VALUES
  ('10000000-0000-4000-9000-000000000040', 412, 351, 34, 96, 92000, 84.0,
   0.852, 0.082, 0.815, 1.9, now() - INTERVAL '90 days', now()),
  ('10000000-0000-4000-9000-000000000041', 288, 236, 29, 71, 54000, 79.0,
   0.819, 0.101, 0.771, 1.2, now() - INTERVAL '120 days', now()),
  ('10000000-0000-4000-9000-000000000042', 173, 121, 31, 48, 38000, 71.0,
   0.699, 0.179, 0.628, 2.6, now() - INTERVAL '30 days', now())
ON CONFLICT (media_id) DO UPDATE SET
  plays = EXCLUDED.plays,
  quality = EXCLUDED.quality,
  trend = EXCLUDED.trend;

COMMIT;

-- To remove everything this file created:
--   DELETE FROM media WHERE id LIKE '10000000-0000-4000-9000-%';
--   DELETE FROM genres WHERE id LIKE '00000000-0000-4000-9000-%';
