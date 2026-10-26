-- 0073_chi_cache_tool_policy.sql
-- Per-run tool policy (`chi_run` opts.toolPolicy), stored with the run so
-- `chi_resume` re-applies it.
--
--   policy_run   1 for a run started with a tool policy. The durable "this run
--                is limited" bit: a policy run whose tool_policy is missing or
--                unreadable must FAIL CLOSED on resume, never run unrestricted.
--   tool_policy  the validated policy as JSON (camelCase); NULL for ordinary
--                runs. Never returned by chi_status / chi_list (not in their
--                column lists).
ALTER TABLE chi_cache ADD COLUMN policy_run INTEGER NOT NULL DEFAULT 0;

ALTER TABLE chi_cache ADD COLUMN tool_policy TEXT;
