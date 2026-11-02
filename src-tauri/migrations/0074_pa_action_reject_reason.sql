-- 0074_pa_action_reject_reason.sql
-- Why the operator rejected a draft (outbox redesign D-05, plans/outbox-redesign).
--
--   reject_reason  free text from `pa_actions_reject { reason }` (host.paActions.reject),
--                  trimmed and capped by the store; NULL when none was given and on
--                  every row that was never rejected. Returned by pa_actions_list as
--                  `rejectReason`. Display only: nothing reads it to decide a send.
ALTER TABLE pa_action_drafts ADD COLUMN reject_reason TEXT;
