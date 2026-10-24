---
"ikenga-desktop": patch
---

In a shared project, the Outbox now shows "Approved by @name, sending" on a draft the box's approvals ledger has already approved (a Mattermost thumbs-up, say) instead of leaving it looking unapproved until the send worker catches up. Such a draft has no Approve button and is skipped by Approve all. This is display only: the box only annotates the list, and approving or sending works exactly as before. A waiting app update ("A new version of Ikenga is ready. Reload to update") is now a persistent banner across the top of the app until you reload, instead of one notice in a queue that other notices could hide. A share viewer no longer sees "Couldn't read the 'resume terminals' setting (forbidden)".
