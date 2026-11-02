---
"ikenga-desktop": minor
---

The Outbox is now one inbox in the outbound pkg. The shell's approve-gate panel is retired, and `/outbox/approvals` (plus the rail, status bar, ⌘K and menu entries) deep-links into the pkg.

- **Saved edits merge.** They no longer replace earlier edits.
- **Reschedule** moves `scheduled_at`.
- **Approving folds every saved edit into the payload**, so the send worker sends what the reviewer saw.
- **Reject reasons are stored** in `reject_reason` (migration 0074).
- **New `host.paActions.list` verb**, routed through the box broker inside a share. Commit carries `seen`, and reject carries an optional reason.
