#!/usr/bin/env bash
# Nightly logical backup with 14-day retention. Schedule via cron/systemd timer.
set -euo pipefail

BACKUP_DIR="${BACKUP_DIR:-./ops/backups}"
CONTAINER="${CONTAINER:-$(basename "$PWD")-postgres-1}"
STAMP=$(date -u +"%Y%m%dT%H%M%SZ")
TARGET="$BACKUP_DIR/space-$STAMP.sql.gz"

mkdir -p "$BACKUP_DIR"

echo "[backup] dumping to $TARGET"
docker exec "$CONTAINER" pg_dump -U postgres -d space | gzip > "$TARGET"

echo "[backup] verifying gzip integrity"
gzip -t "$TARGET"

echo "[backup] pruning backups older than 14 days"
find "$BACKUP_DIR" -name 'space-*.sql.gz' -mtime +14 -delete

echo "[backup] done: $TARGET ($(du -h "$TARGET" | cut -f1))"
