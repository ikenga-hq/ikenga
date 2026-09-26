// Mac-only native menu wiring (D-08 `native-menu`). Tauri 2 exposes Menu/
// Submenu/MenuItem/PredefinedMenuItem from `@tauri-apps/api/menu` and lets us
// set the application menu from JS. If the API isn't available (older Tauri
// version, or capability not granted), we log + bail. JS-side keyboard
// shortcuts in the workspace + command palette remain functional regardless.
//
// Windows/Linux get the SAME tree (D-08 `native-menu-win`) as an in-app
// cascading menu behind a `≡` button — `src/shell/menu/cascade.tsx`, mounted
// in `title-row.tsx`. Both renderers read the one definition in
// `src/shell/menu/tree.tsx`; nothing here duplicates menu structure.
//
// WP-55 (menus render from data): every action leaf now comes from
// `getEffectiveMenu('native/<top>')` (`tree.tsx`'s `resolveMenuTree`) — the
// predefined OS roles stay fixed, but reordering, hiding, or a package/user
// append in `actions.json` reaches the menu bar. Rebuilding the whole native
// `Menu` and calling `setAsAppMenu()` again is the only way to reflect that
// (Tauri's menu API has no "patch one submenu" call), so this subscribes to
// the effective model and to every keymap publish (a rebind changes shown
// accelerators) and rebuilds on each. `installNativeMenu()` itself stays
// call-once from `boot/primary.tsx`; the rebuild loop lives inside it.
//
// DEC-58 single fire: a native-menu accelerator and the DOM keydown can both
// reach the same command for one press. `menuItemAction` (`commands.ts`)
// dedupes them — the registry (`runCommand`, via `activateActionId`) stays
// the one thing that actually runs a command.

import { subscribeEffectiveModel } from '@/lib/actions/store';
import { menuItemAction } from '@/lib/keymap/commands';
import { subscribeKeymap } from '@/lib/keymap/registry';
import { isMac } from '@/lib/platform';
import { isTauri } from '@/lib/transport';
import {
	activateActionId,
	macAccelerator,
	MENU_TREE,
	type MenuDef,
	type PredefinedKind,
	type ResolvedMenuEntry,
	resolveMenuTree,
} from './menu/tree';

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

let installed = false;

export async function installNativeMenu(): Promise<void> {
	if (!isTauri()) {
		console.log(
			'[transport] api/menu (native menu) is desktop-only (in-DOM fallback undesigned) — deferred to Wave 2'
		);
		return;
	}
	if (!isMac) return;
	if (installed) return;
	installed = true;

	try {
		// Lazy import — module may not be present if Tauri capabilities aren't
		// wired for menu access yet.
		const menuMod = (await import('@tauri-apps/api/menu')) as typeof import('@tauri-apps/api/menu');
		const { Menu, Submenu, MenuItem, PredefinedMenuItem } = menuMod;

		async function buildEntry(entry: ResolvedMenuEntry) {
			if (entry.kind === 'separator') return PredefinedMenuItem.new({ item: 'Separator' });
			if (entry.source === 'role') {
				return PredefinedMenuItem.new({ item: predefinedItemOption(entry.predefined) });
			}
			return MenuItem.new({
				id: entry.id,
				text: entry.label,
				accelerator: macAccelerator(entry.id),
				action: menuItemAction(entry.id, () => activateActionId(entry.id)),
			});
		}

		async function buildSubmenu(menu: MenuDef) {
			const items = await Promise.all(resolveMenuTree(menu).map(buildEntry));
			return Submenu.new({ text: menu.label, items });
		}

		async function rebuild() {
			const submenus = await Promise.all(MENU_TREE.map(buildSubmenu));
			const menu = await Menu.new({ items: submenus });
			await menu.setAsAppMenu();
		}

		await rebuild();
		// G-55 (D-08 state map): the OS draws this menu, so there's no DOM
		// subtree of its own to tag — mark the document root as the closest
		// inspectable "rendered root" for the `native-menu` state, mirroring
		// `native-menu-win`'s `data-state` on the cascade's own root below.
		document.documentElement.dataset.state = 'native-menu';

		// Reordering/hiding an item in `actions.json` (the effective model) or
		// rebinding a key (accelerator labels) must reach the menu bar without
		// a restart (G-ACTIONS §Phase 6 verification — "Menus" section).
		subscribeEffectiveModel(() => {
			void rebuild().catch((err) => console.warn('[native-menu] rebuild failed:', err));
		});
		subscribeKeymap(() => {
			void rebuild().catch((err) => console.warn('[native-menu] rebuild failed:', err));
		});
	} catch (err) {
		// TODO: native menu wiring requires the Tauri menu plugin + capability.
		// Falling back to JS-side keyboard shortcuts in the workspace.
		// eslint-disable-next-line no-console
		console.warn('[native-menu] could not install, falling back to JS shortcuts', err);
	}
}
