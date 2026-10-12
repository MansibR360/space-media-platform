#!/usr/bin/env bash
# Restore a backup into a fresh database. Run restore TESTS monthly (see SECURITY.md).
set -euo pipefail

FILE="${1:?usage: restore.sh <path-to-space-*.sql.gz>}"
CONTAINER="${CONTAINER:-$(basename "$PWD")-postgres-1}"

echo "[restore] stopping app to avoid writes"
docker compose stop app || true

echo "[restore] recreating database"
docker exec -i "$CONTAINER" psql -U postgres -d postgres <<'SQL'
SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = 'space';
DROP DATABASE IF EXISTS space;
CREATE DATABASE space;
SQL

echo "[restore] restoring from $FILE"
gunzip -c "$FILE" | docker exec -i "$CONTAINER" psql -q -U postgres -d space

echo "[restore] sanity checks"
COUNT=$(docker exec "$CONTAINER" psql -U postgres -d space -tAc "SELECT COUNT(*) FROM users;")
echo "[restore] users in restored db: $COUNT"

echo "[restore] starting app again"
docker compose start app || true
echo "[restore] done — verify /api/health and spot-check catalog"
