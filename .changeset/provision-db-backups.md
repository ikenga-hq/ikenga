---
"ikenga-desktop": minor
---

`provision.sh backups`: Postgres backups as hardened system jobs on a server box — a dedicated `ikenga-backup` user (connection strings from a reserved `backup` secrets scope, GCS service-account key), `pg_dump` 17 from PGDG and `gcloud storage`, per-schedule systemd timers (4-hourly / daily / weekly, UTC), dump → verify → upload to `gs://<bucket>/<db>/<YYYY>/<MM>/`, and a secret-free `status.json` for alerting. Root never follows or writes through a path the backup user controls; disabling removes timers, connection strings and cached credentials.
