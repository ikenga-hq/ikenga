---
"ikenga-desktop": minor
---

**The Store installs git and npx primitives, pinned to what you reviewed (D-02 addendum, Round 57).**

- **Signed-catalog rows in the Store.** The curated `primitives.json` entries list beside the registry pkgs, with **Source** chips (registry · git · npx) and a `hook` kind. When a catalog entry and a registry pkg share a kind and name, one row shows: the registry row, with an `also: npx` note. A catalog row's sheet shows the **requires** closure before consent, one Share-kola box per dependency that isn't in the catalog, the trust facts, and **Install to \<project\> ▾**.
- **Add from URL…** beside the search box (and from an empty search, carrying the query): paste a git URL or an `owner/repo` spec, **Resolve** to see the kind, commit, files and closure *before* anything is written, acknowledge the unsigned source, then install — exactly the commit that was resolved.
- **Pinned installs.** Catalog entries can now carry a pinned commit (`ref`) and content `hash`. Catalog installs follow the catalog's pin instead of HEAD, and join the Updates strip when the pin moves. An install or update that no longer matches the reviewed commit or content is refused, and nothing is written.
- **Installed: Update and Remove… for git/npx items.** Update shows `<installed> → <remote>` and confirms before fetching exactly that commit. Remove… is dependents-aware: it lists every link and every item that requires it, then lets you unlink and delete, relink to another copy and delete, or forget the record and keep every file (the item then shows as `local`).
- **Hooks and MCP servers install from git** (as settings fragments), from the catalog or a URL.
- Backend: new `oba_resolve_source` dry-run; `oba_install_git` / `oba_install_npx` / `oba_install_with_deps` / `oba_update` accept `expectSha` / `expectHash`; `oba_auto_update_all` takes the catalog pins; git runs with credential prompts disabled (public sources only).
