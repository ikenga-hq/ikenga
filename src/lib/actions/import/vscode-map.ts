// WP-61 — frozen lookup tables and shared types for the Import surface
// (G-ACTIONS §1.2, §5, §10). None of this is authored by a user; it ships
// with the shell and is versioned like any other source file.
//
// ── VS Code command → Ikenga id map (the "id-map core") ─────────────────────
// Scope decision (user, 2026-09-26, `04-discussion.md` Round 42): the VS Code
// import maps only this small, frozen core — every other VS Code `command`
// is reported "not imported" with a reason (G-ACTIONS §14 item 24). Growing
// this table is a later round, not something the import surface infers on
// its own. A `null` value is a command WP-61 recognizes but that has no
// Ikenga equivalent (shown with that reason, not "unrecognized") — the
// `workbench.action.terminal.new` row is G-ACTIONS §10.5's own worked
// example ("skipped, no equivalent").
export const VSCODE_COMMAND_MAP: Readonly<Record<string, string | null>> = {
	// "Go to file…" → the built-in Project switcher (G-ACTIONS §10.5 D-06 ids
	// table: "(none) VS Code 'Go to file…' maps onto `palette.projects`").
	'workbench.action.quickOpen': 'palette.projects',
	// "Toggle sidebar" → `explorer.toggle` (§10.2: "VS Code 'Toggle sidebar'
	// import loses to it" — `mod+b` is already held by the default).
	'workbench.action.toggleSidebarVisibility': 'explorer.toggle',
	// The Command Palette itself.
	'workbench.action.showCommands': 'palette.open',
	// §10.5's own "no equivalent" example.
	'workbench.action.terminal.new': null,
};

/** A VS Code `when` context key this import recognizes, mapped onto its
 *  DEC-62 (G-ACTIONS §4.3) equivalent. Deliberately tiny: the id-map core's
 *  four commands ship with no `when` in VS Code's own defaults, so this
 *  table mostly exists to prove the parser's `when`-subset claim rather than
 *  to cover VS Code's full context-key surface. Anything not listed here
 *  makes the row's `when` "unsupported" (§15 A — the row is skipped with a
 *  reason, never silently widened to `always`). */
export const VSCODE_WHEN_KEY_MAP: Readonly<Record<string, string>> = {
	terminalFocus: 'terminalFocus',
	editorTextFocus: 'inputFocus',
	editorFocus: 'inputFocus',
	sideBarFocus: 'explorerFocus',
	explorerViewletFocus: 'explorerFocus',
	explorerViewletVisible: 'explorerFocus',
};

/** G-ACTIONS §1.2: "D-06's glyph ids (`chi`, `bolt`, `refresh`, §11.7) are
 *  not Lucide names; WP-61's import maps them through an alias table it
 *  owns … and anything left unknown falls back as above [renders `zap`]."
 *  Applied to every icon string an import candidate carries before it is
 *  written, so a value copied from a D-06-style export doesn't collapse to
 *  the generic fallback for no reason. */
export const ICON_GLYPH_ALIASES: Readonly<Record<string, string>> = {
	bolt: 'zap',
	chi: 'send',
	refresh: 'refresh-cw',
	term: 'terminal',
};

/** Resolves a candidate's `icon` field through the alias table above. Passes
 *  through anything not in the table unchanged — `ActionIcon` (shared, WP-57)
 *  already warns and falls back to `zap` for a name that still isn't a known
 *  Lucide icon after this. */
export function resolveImportIcon(icon: string | undefined | null): string | undefined {
	if (!icon) return undefined;
	return ICON_GLYPH_ALIASES[icon] ?? icon;
}

/** One row of an Import diff (G-ACTIONS §5 "Import is a writer, not a
 *  layer"; the routine brief: "add, skip … or clash"). `clash` never means
 *  "written without the action" — it means the action or binding this row
 *  represents is kept out of the write entirely, or written without its key,
 *  because writing it would override something already there (§5's "an
 *  import never writes a rule that overrides an existing binding"). */
export type ImportRowKind = 'add' | 'skip' | 'clash';

export interface ImportDiffRow {
	kind: ImportRowKind;
	/** Display name for the diff list (`diffrow .mid2 > b`, D-06). */
	title: string;
	/** Secondary line (`diffrow .t2`, D-06) — always present for `skip` /
	 *  `clash`, since every non-add row must say why (routine DoD: "unmapped
	 *  commands are skipped with a reason"). */
	detail: string;
	/** Idempotency / row identity for React keys and re-render diffing. */
	key: string;
}
