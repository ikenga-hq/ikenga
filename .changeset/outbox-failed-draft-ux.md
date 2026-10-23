---
"ikenga-desktop": patch
---

The Outbox approvals screen no longer offers Approve & Send on a draft that has already failed, been approved, or been sent (it now says why: "failed to send, use Retry" or "cannot be approved again", "already approved, waiting for the send worker", "sending now"). A network failure while approving shows "Couldn't reach the box (network error)…" with what to do, instead of the browser's raw "Failed to fetch"; an HTTP error from the box is explained the same way.
