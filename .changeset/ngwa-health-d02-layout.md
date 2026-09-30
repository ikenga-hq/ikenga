---
"ikenga-desktop": patch
---

Ngwa Health now matches its D-02 design, and removing broken pkgs works.

- The six panels (Violations, Sidecars, Cron, Data, Trust, Engines) grow to fit their rows, and the page scrolls. Panels no longer clip their rows or hide buttons such as "Last backup" and "Reinstall from registry".
- Violation rows carry their actions inline. A pkg in the pkgs folder that failed to load offers Reinstall from registry (when the registry lists it), Remove… and Hand to Chi.
- Trust is its own panel.
- Remove on a folder that failed to load moves it to a recoverable `.uninstalled-…` backup, as uninstall does, instead of deleting it.
- Remove all covers everything the scan lists, folders included. It rescans afterwards and says exactly what it removed and what is left, and why. It no longer reports "done" while an issue remains.
