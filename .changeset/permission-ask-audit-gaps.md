---
"ikenga-desktop": patch
---

Terminal permission asks that end with nobody deciding them now always leave an audit record. This covers a gate that times out on the desktop, a decision made when its row is gone, and an ask left open when the daemon or the desktop restarted. The restart record is written again on the next start until it really lands, so a crash or a failed write cannot lose it; the cost is that it can occasionally be written twice. On a multi-account box the broker now tells each account's process exactly which records it saved, instead of marking every waiting row done. A box or desktop with nowhere to record no longer retries these forever at every start.
