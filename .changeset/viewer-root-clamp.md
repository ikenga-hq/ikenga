---
"ikenga-desktop": patch
---

Close the viewer-mount widening hole in browser sessions. The preview root is computed from the page's own `../` references, so a hostile HTML file in a shared project could widen it to the whole home and `fetch()` credentials out of it. `viewer_serve` now takes the page being previewed (`filePath`) and refuses, with an error rather than a silent clamp, any root above that page's project root from the daemon's own project registry (or, for a page in no project, above its own directory; a project rooted at the home directory bounds nothing and is ignored). The preview pane clamps its request to the project root too, so pages that reach shared assets inside the project keep working. The `/__viewer` CSP no longer allows `https:` for `img-src` and `media-src`, which closes the remaining channel for sending what a page read to an outside host.
