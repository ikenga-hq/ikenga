-- 0068_chi_cache_runner_pid.sql
-- WP-18b (ADR-023 D4/D5, remote-access G-85..G-89): tmux retires. A
-- persistent Chi run is now a detached `chi-runner` process (its own process
-- group) instead of a tmux session, so the row records the runner's OS pid
-- rather than a tmux session name.
--
--   pid  chi-runner's pid while the run is detached; NULL for in-process
--        runs (and cleared when a detached run is resumed in-process). The
--        reconciliation sweep reads it with the run's status file to decide
--        running / done / failed after an app restart.
--
-- `terminal_session_id` held the tmux session name and has no reader left.
-- DROP COLUMN needs SQLite >= 3.35 (the bundled libsqlite3-sys ships 3.46)
-- and a column no index / view / trigger references — none does.
ALTER TABLE chi_cache ADD COLUMN pid INTEGER;

ALTER TABLE chi_cache DROP COLUMN terminal_session_id;
