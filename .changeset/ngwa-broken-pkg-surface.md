---
"ikenga-desktop": patch
---

A pkg that is on disk but fails to register at boot (for example a manifest still on `ui.nav`) is no longer invisible. Its rail pins are hidden (not deleted) until it registers again, so the rail never shows a dead icon. Ngwa Health now lists it with the parse error, from `pkg_health_scan`'s new `pkgs_dir_unloadable` and `register_failed` kinds, and offers "Reinstall from registry" (or Remove, which deletes the unloadable folder). The Store row reads "installed · failed to load" with a Reinstall action that goes through the normal consent sheet.
