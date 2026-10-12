# Point-in-Time Recovery (PITR) Guide

WAL-based PITR lets you restore the database to *any second* in the past, not
just to the nightly dump. This is the difference between losing 24 hours and
losing 30 seconds.

## 1. Enable WAL archiving (Postgres 15+)

`postgresql.conf`:

```
archive_mode = on
archive_command = 'test ! -f /backups/wal/%f && cp %p /backups/wal/%f'
wal_level = replica
max_wal_senders = 3
```

Mount `/backups/wal` on persistent storage (same volume as nightly dumps).

For Docker compose, add under the postgres service:

```yaml
command:
  - postgres
  - -c
  - archive_mode=on
  - -c
  - archive_command=cp %p /backups/wal/%f
  - -c
  - wal_level=replica
```

## 2. Baseline backup

PITR replays WAL **on top of a base backup**:

```bash
docker exec space-postgres-1 pg_basebackup -U postgres -D /backups/base -Fp -Xs -P
```

Schedule it weekly alongside the nightly `ops/backup.sh`.

## 3. Restore to a point in time

```bash
docker compose stop app

# restore base backup into the data directory, then configure recovery:
touch /var/lib/postgresql/data/recovery.signal
cat >> /var/lib/postgresql/data/postgresql.auto.conf <<EOF
restore_command = 'cp /backups/wal/%f %p'
recovery_target_time = '2026-01-14 09:32:00+00'
recovery_target_action = 'promote'
EOF

docker compose start postgres
```

Postgres replays WAL until the target time and promotes. Remove the recovery
settings afterwards.

## 4. Verification drill (run monthly)

1. Restore to a fresh volume at T-1h.
2. `SELECT COUNT(*) FROM users; SELECT COUNT(*) FROM media;`
3. Boot the app against the restored DB; hit `/api/ready`.
4. Record results in your incident log. An unverified backup is not a backup.

## 5. What is NOT covered by PITR

Media files (`media`, `storage` volumes) live outside Postgres. Snapshot those
volumes separately on the same schedule; pair each media snapshot with the
nearest DB base backup so IDs stay consistent.
