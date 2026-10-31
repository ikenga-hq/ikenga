---
'ikenga-desktop': patch
---

Explorer Views now shows an Outbox entry (with the pending-approvals count) automatically when an approvals backend is active: the `com.ikenga.outbound` pkg installed and enabled (or drafts present) on desktop, the sender approvals socket live on a remote box, and operators only inside a share. Adds the read-only `outbox_approvals_status` RPC arm.
