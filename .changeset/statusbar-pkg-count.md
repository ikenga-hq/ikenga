---
"ikenga-desktop": patch
---

The status bar's middle Ngwa group now labels its install count "N pkgs" (singular "1 pkg") instead of "N installed", because it only ever counted the pkg kernel's installed rows, not the full Ngwa catalogue (skills, agents, hooks, mcp tools, …) the label implied. The Ngwa Installed tab now shows a small muted "· N pkgs" sub-count next to its total, computed by the same shared `selectPkgCount` definition, so the two numbers always agree.
