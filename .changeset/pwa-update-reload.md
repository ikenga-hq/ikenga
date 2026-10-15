---
'ikenga-desktop': patch
---

Fix "Reload to update" in the browser app. The button could do nothing at all when the new version had already taken over (for example after updating from another tab), leaving the banner stuck on an old front end. It now reloads in that case, and if another tab updates, this tab shows the banner so you can reload when ready instead of silently running old code. The reload is also no longer blocked by the "Leave site?" prompt about open terminals (they live on the server and come back), but it still asks first if you have unsaved edits.
