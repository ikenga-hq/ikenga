---
"ikenga-desktop": minor
---

Approve drafts from inside a share. In a share of an agent's project (on the box: Ruby jobs) an Operator can open `/outbox/approvals`, edit, approve and reject the owner's drafts. A Reviewer or Guest gets a clear "needs the Operator role" message and nothing else.

Approving records a ledger approval as the authenticated viewer, written by the broker (root), never by the owner's daemon: the broker checks the role, asks the sender's approvals socket for the draft, recomputes the canonical hash of the row the viewer was shown (contract v1, pinned by its test vectors) and only then sends the `source: "shell"` approve message and tells the owner's daemon to commit. No sender socket, a refusal, or a draft that changed since it was shown means nothing is approved and the error appears on the draft. Socket path: `IKENGA_APPROVALS_SOCKET` (default `/run/ikenga-sender/approvals.sock`). `pa_actions_list/update/commit/reject` are now share-reachable (same capabilities as before); `pause` and `retry` stay owner-only. Desktop and own-workspace behaviour is unchanged.
