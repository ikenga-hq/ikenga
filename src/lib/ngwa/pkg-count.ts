// Shared "pkg count" definition — DEC-73 (Round 58,
// `plans/shell-ux-rearchitecture/04-discussion.md`).
//
// The status bar's Ngwa segment reads `usePkgsDerived().installed.length`
// straight off the kernel (`pkg_installed` rows, all scopes) — every row
// there already IS a pkg, so it needs no further filtering. The Ngwa
// Installed tab reads the full `ngwa_snapshot` (skills, agents, hooks, mcp
// tools, pkgs, …), so it needs this selector to pick the sub-set that
// corresponds 1:1 to those same kernel rows.
import type { NgwaItem, NgwaKind } from '@ikenga/contract';

/** The `NgwaKind`s the pkg kernel can produce from a `pkg_installed` row:
 *  `pkg_kind()` (`shell/src-tauri/src/commands/ngwa.rs`) derives exactly one
 *  of these per installed manifest (priority engine → app → tool → sidecar;
 *  `tool` also covers a pkg's own `mcp[]` block). Everything else (`skill`,
 *  `agent`, `command`, `hook`, `bundle`, `schedule`, `workflow`) comes from
 *  Ọba or the engine-config scan, never the kernel. */
export const PKG_NGWA_KINDS: ReadonlySet<NgwaKind> = new Set<NgwaKind>([
	'app',
	'engine',
	'tool',
	'sidecar',
]);

/**
 * `tool` is the one kind the kernel does NOT own exclusively: a pkg's own
 * `mcp[]` server gets registered into `~/.claude.json` under
 * `pkg-<slug>-<server>`, and the engine-config scan (`scan_rows()` in
 * `ngwa.rs`) turns that registration into its OWN `tool` item too — same as
 * it does for a hand-configured or Ọba-managed MCP server that no pkg owns.
 * Counting every `tool` item would double-count an installed mcp pkg (once
 * for the pkg's own row, once for its registered server) and single-count a
 * standalone one that isn't a pkg at all — exactly the drift DEC-73 fixes.
 *
 * The frozen Ngwa-item contract's own `id` doc comment is the tell: "Stable
 * across a refresh. Pkg-backed: the manifest id. Everything else:
 * `${kind}:${scope_key}:${name}`" (`@ikenga/contract`'s `ngwa.ts`). A
 * pkg-backed row's id is the raw manifest id (reverse-DNS, e.g.
 * `com.ikenga.mcp-iyke`); every scan-sourced or Ọba-sourced row's id is that
 * `${kind}:${scope}:${name}` template, which always starts with `${kind}:`.
 * `app` / `engine` / `sidecar` never arise outside the kernel row, so this
 * check is a no-op for them — it only ever excludes a non-pkg `tool`.
 */
function isPkgBacked(it: NgwaItem): boolean {
	return !it.id.startsWith(`${it.kind}:`);
}

/** The pkg sub-count for a set of Ngwa items — must equal the status bar's
 *  `usePkgsDerived().installed.length` for the same underlying install set. */
export function selectPkgCount(items: readonly NgwaItem[]): number {
	let n = 0;
	for (const it of items) {
		if (PKG_NGWA_KINDS.has(it.kind) && isPkgBacked(it)) n++;
	}
	return n;
}
