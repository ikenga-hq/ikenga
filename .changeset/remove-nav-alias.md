---
"ikenga-desktop": minor
---

**BREAKING: `ui.nav` removed (DEC-37 cutover).** The `ui.nav` → `ui.views`
alias had a one-release lifetime (G-MANIFEST-V5 §4) and v0.12.0 was the
soft-warn release, so the Rust manifest parser now rejects `ui.nav` outright
with a canonical message naming `ui.views[]`. `Manifest::apply_nav_views_alias`,
`NavAliasOutcome` and `normalize_nav_route` are gone; `Package::load` does no
post-parse fixup. The `NavEntry` wire shape survives on the activity-bar
registry snapshot (pkg-mode sidebar + WP-22 pin seed read it), sourced from
`ui.views[]`.
