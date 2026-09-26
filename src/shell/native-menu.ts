// Mac-only native menu wiring (D-08 `native-menu` — WP-46 completes this to
// the full nine-menu tree: Ikenga · File · Edit · View · Project · Chi ·
// Ngwa · Window · Help). Tauri 2 exposes Menu/Submenu/MenuItem/
// PredefinedMenuItem from `@tauri-apps/api/menu` and lets us set the
// application menu from JS. If the API isn't available (older Tauri version,
// or capability not granted), we log + bail. JS-side keyboard shortcuts in
// the workspace + command palette remain functional regardless.
//
// Windows/Linux get the SAME tree (D-08 `native-menu-win`) as an in-app
// cascading menu behind a `≡` button — `src/shell/menu/cascade.tsx`, mounted
// in `title-row.tsx`. Both renderers read the one definition in
// `src/shell/menu/tree.tsx`; nothing here duplicates menu structure.
//
// ── Shipped vs added (WP-46) ────────────────────────────────────────────────
// Shipped before this file changed (unchanged behaviour, including the two
// pre-existing dead `CustomEvent`s noted below — WP-46's scope is the menu
// TREE, not auditing every existing handler):
//   File    → New Session (⌘N) — FIXED (see below), Open File… (⌘O),
//             Open Project Folder… (⌘⇧O)
//   Edit    → Undo/Redo/Cut/Copy/Paste/Select All (all `PredefinedMenuItem`)
//   Session → New Terminal (⌘T) — FIXED (see below), Switch Adapter
//             (coming soon) (⌘⇧A) — genuinely not-yet-built, left alone
//             (this menu is renamed to **Chi** per D-08; same two items)
//   View    → Command Palette (⌘K) — FIXED (see below)
//   Window  → Minimize/Maximize/Fullscreen (all `PredefinedMenuItem`)
//   Help    → Docs, Send Feedback
// Added (net new menus/items, D-08 tree): the whole **Ikenga** app menu
// (About/Check for Updates/Settings/Hide/Quit), **Project** menu, **Ngwa**
// menu, and inside existing menus: File → Close Tab; View → Toggle Explorer,
// Toggle Companion, Split Right, Split Down; Chi → Permission Inbox, Runs;
// Help → Keyboard Shortcuts. Every added item either reuses an existing
// WP-08 registry command (so its key is whatever that command is already
// bound to) or ships with no key at all — **no new bindings** (Phase 6 owns
// rebinding). Design items with neither a registry id nor a real existing
// action (New Tab, Screenshot pane…, Zoom In/Out/Reset Zoom, Open Recent,
// Reveal in Files, Reset Layout, Dispatch…, Check for package updates,
// Cultural attribution) are omitted — see the WP-46 PR body for the full list.
//
// ── Found while completing this tree, and what happened to each ────────────
// A repo-wide grep turned up FOUR pre-existing dead `CustomEvent`s — nothing
// anywhere subscribes to `cmd:new-terminal` / `cmd:switch-adapter` /
// `cmd:open-file` / `cmd:open-project-folder` / `cmd:open-command-palette`,
// so this file's own header comment claiming "Listeners (terminal-eng,
// agent-eng, etc.) subscribe to the event names below" was stale. Three are
// fixed here (real, already-shipped store calls / helpers replace the dead
// dispatch — see each `tree.tsx` item's own comment for the call site):
// File → New Session, Chi → New Terminal (both via `newSessionTab()`), and
// View → Command Palette (via `openCommandPalette('all')`). `Open File…` /
// `Open Project Folder…` are left as shipped (dead `EVT.openFile` /
// `EVT.openProjectFolder`) — no equally-confident real replacement action
// was identified (see `tree.tsx`); `Switch Adapter (coming soon)` is left
// alone because it's a genuine not-yet-built feature, not a bug.

import { menuItemAction } from '@/lib/keymap/commands';
import { isMac } from '@/lib/platform';
import { isTauri } from '@/lib/transport';
import { MENU_TREE, macAccelerator, type MenuDef, type MenuLeaf, type PredefinedKind } from './menu/tree';

const EVT = {
	openFile: 'cmd:open-file',
	openProjectFolder: 'cmd:open-project-folder',
	switchAdapter: 'cmd:switch-adapter',
} as const;

function emit(name: string) {
	window.dispatchEvent(new CustomEvent(name));
}

/** Ids whose action is a pre-existing, unchanged shipped behaviour that isn't
 *  expressed as a plain `action()` in `tree.tsx` (the two still-dead event
 *  dispatches this file owns, the intentionally-unimplemented stub, plus the
 *  two external links). Keyed by the leaf's `id`. */
const SHIPPED_MAC_ACTIONS: Record<string, () => void> = {
	'open-file': () => emit(EVT.openFile),
	'open-project': () => emit(EVT.openProjectFolder),
	'switch-adapter': () => emit(EVT.switchAdapter),
	docs: () => {
		window.open('https://royalti.io/docs', '_blank');
	},
	feedback: () => {
		window.open('mailto:feedback@royalti.io?subject=Royalti%20PA%20Feedback', '_blank');
	},
};

function leafAction(leaf: MenuLeaf): (() => void) | undefined {
	return leaf.action ?? SHIPPED_MAC_ACTIONS[leaf.id];
}

/** Tauri's `PredefinedMenuItemOptions['item']` for a tree `PredefinedKind`. */
function predefinedItemOption(
	kind: PredefinedKind
):
	| 'Undo'
	| 'Redo'
	| 'Cut'
	| 'Copy'
	| 'Paste'
	| 'SelectAll'
	| 'Minimize'
	| 'Maximize'
	| 'Fullscreen'
	| 'Hide'
	| 'Quit'
	| { About: null } {
	switch (kind) {
		case 'undo':
			return 'Undo';
		case 'redo':
			return 'Redo';
		case 'cut':
			return 'Cut';
		case 'copy':
			return 'Copy';
		case 'paste':
			return 'Paste';
		case 'selectAll':
			return 'SelectAll';
		case 'minimize':
			return 'Minimize';
		case 'maximize':
			return 'Maximize';
		case 'fullscreen':
			return 'Fullscreen';
		case 'hide':
			return 'Hide';
		case 'quit':
			return 'Quit';
		case 'about':
			return { About: null };
	}
}

export async function installNativeMenu(): Promise<void> {
	if (!isTauri()) {
		console.log(
			'[transport] api/menu (native menu) is desktop-only (in-DOM fallback undesigned) — deferred to Wave 2'
		);
		return;
	}
	if (!isMac) return;

	try {
		// Lazy import — module may not be present if Tauri capabilities aren't
		// wired for menu access yet.
		const menuMod = (await import('@tauri-apps/api/menu')) as typeof import('@tauri-apps/api/menu');
		const { Menu, Submenu, MenuItem, PredefinedMenuItem } = menuMod;

		async function buildItem(leaf: MenuLeaf) {
			if (leaf.predefined) {
				return PredefinedMenuItem.new({ item: predefinedItemOption(leaf.predefined) });
			}
			const action = leafAction(leaf);
			return MenuItem.new({
				id: leaf.id,
				text: leaf.label,
				accelerator: macAccelerator(leaf.commandId),
				// A structure-only item (no registry id, no real handler — see
				// the header comment for the full omitted-vs-added tally) still
				// renders so the tree matches D-08; it just does nothing on click.
				// DEC-58 (WP-54): the accelerator stays visible, but one press
				// reaches the command once — if the key dispatcher already ran
				// this command for the same press (or runs it right after), the
				// second path is dropped (`claimSingleFire`), so the registry
				// stays the one firing path.
				action: menuItemAction(leaf.commandId, action ?? (() => {})),
			});
		}

		async function buildSubmenu(menu: MenuDef) {
			const items = [];
			for (const entry of menu.items) {
				if (entry.kind === 'separator') {
					items.push(await PredefinedMenuItem.new({ item: 'Separator' }));
				} else {
					items.push(await buildItem(entry));
				}
			}
			return Submenu.new({ text: menu.label, items });
		}

		const submenus = await Promise.all(MENU_TREE.map(buildSubmenu));
		const menu = await Menu.new({ items: submenus });
		await menu.setAsAppMenu();
		// G-55 (D-08 state map): the OS draws this menu, so there's no DOM
		// subtree of its own to tag — mark the document root as the closest
		// inspectable "rendered root" for the `native-menu` state, mirroring
		// `native-menu-win`'s `data-state` on the cascade's own root below.
		document.documentElement.dataset.state = 'native-menu';
	} catch (err) {
		// TODO: native menu wiring requires the Tauri menu plugin + capability.
		// Falling back to JS-side keyboard shortcuts in the workspace.
		// eslint-disable-next-line no-console
		console.warn('[native-menu] could not install, falling back to JS shortcuts', err);
	}
}
