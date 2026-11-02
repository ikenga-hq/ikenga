---
"ikenga-desktop": minor
---

Outbox round 2 (plans/outbox-redesign D-15, D-16, G-CONTRACT r2).

- **Email attachments.** `host.paActions.update` accepts `attachments`: up to 20 `{ url, name? }` entries, each URL https. The key is type-checked by the iframe host and by the Rust store (also in the box broker, before the row lookup), merged into `edited_json`, and folded into `item.attachments` on approve.
- **Social-queue posts are edited at the source.** An edit to a draft whose item carries `social` no longer touches `edited_json`. The patch (`body`, `firstComment`, `media`, `scheduledIso` only) is applied to `item.social`, the Buffer fields are re-derived exactly as the sender's `buildSocialItem` derives them, and the draft (`awaiting` → `edited`) and its linked `social_queue` row (`content`, `media_url`, `scheduled_for`) are written in one transaction. Edits are checked against the sender's rules and refused as `social_refused: <code>: …` using the sender's problem codes (`over_280`, `x_no_first_comment`, `bad_media_url`, `bad_scheduled_for`, …). X threads are read-only. Golden fixtures generated from the sender's own code pin the derivation.
