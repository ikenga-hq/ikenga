---
"ikenga-desktop": patch
---

Opening a shared project no longer drops you into onboarding. In a share every request goes to the Owner's account, so the shell was loading the Owner's personal settings: an Owner who never onboarded (a bot account, say) sent every viewer to the first-run wizard, and changing the theme in a share would have changed the Owner's. While a share is open the shell now keeps the viewer's own settings and writes none.
