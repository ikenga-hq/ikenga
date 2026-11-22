---
"ikenga-desktop": patch
---

Outside links now answer every unusual request the same way. An OPTIONS request to a link address got an empty "OK" from the server instead of the usual "This link isn't available" page, and the refusal of a passcode sent from another website lacked the headers that keep link pages out of search engines, browser caches and referrer logs. Both now get the same treatment as every other link answer.
