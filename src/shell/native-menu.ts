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
// Rebuilds are serialized (`createRebuildQueue`): both subscriptions feed one
// queue, requests arriving together coalesce into one build, a build never
// overlaps another, and a generation counter drops a build that a newer
// request superseded — so a stale menu never lands last. The previous menu's
// resources (menu, submenus, items) are closed once the new one is set.
//
// DEC-58 single fire: a native-menu accelerator and the DOM keydown can both
// reach the same command for one press. `menuItemAction` (`commands.ts`)
// dedupes them — the registry (`runCommand`, via `activateActionId`) stays
// the one thing that actually runs a command. The accelerator path also
// honours the binding's `when` (`acceleratorBlocked`): a `!inputFocus`
// command pressed while typing does nothing through the menu, as through its
// key. A mouse click on the item is not gated.

import { subscribeEffectiveModel } from '@/lib/actions/store';
import { DEDUPE_WINDOW_MS, menuItemAction } from '@/lib/keymap/commands';
import { getContextKeys, getEvalOptions } from '@/lib/keymap/context-keys';
import { eventMatchesCombo } from '@/lib/keymap/platform';
import { findEntry, subscribeKeymap } from '@/lib/keymap/registry';
import { evaluateWhen } from '@/lib/keymap/when';
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

// ─── Rebuild queue ────────────────────────────────────────────────────────

/** Something `close()`-able a build created (Tauri `Resource`s). */
export interface ClosableResource {
	close: () => Promise<void>;
}

export interface BuiltMenu {
	/** Installs the built menu as the app menu. */
	apply: () => Promise<void>;
	/** Every resource the build created, closed when it is replaced (or when
	 *  a superseded build is dropped). */
	resources: ClosableResource[];
}

async function closeAll(resources: readonly ClosableResource[]): Promise<void> {
	await Promise.all(resources.map((r) => r.close().catch(() => {})));
}

/**
 * One serialized rebuild loop. `request()` may be called any number of times
 * from any subscription: requests in the same tick coalesce, a build never
 * overlaps another, and a build that finishes after a newer request arrived
 * is dropped (its resources closed) instead of applied — the generation
 * counter guarantees the last menu applied is the newest one.
 */
export function createRebuildQueue(
	build: () => Promise<BuiltMenu>,
	opts: { onError?: (err: unknown) => void; schedule?: (fn: () => void) => void } = {}
) {
	const schedule = opts.schedule ?? ((fn: () => void) => queueMicrotask(fn));
	const onError = opts.onError ?? ((err: unknown) => console.warn('[native-menu] rebuild failed:', err));
	let generation = 0;
	let applied = 0;
	let running: Promise<void> | null = null;
	let scheduled = false;
	let current: ClosableResource[] = [];

	async function loop(): Promise<void> {
		while (applied < generation) {
			const gen = generation;
			let built: BuiltMenu;
			try {
				built = await build();
			} catch (err) {
				onError(err);
				applied = gen;
				continue;
			}
			if (gen !== generation) {
				// Superseded while building: never apply a stale menu.
				await closeAll(built.resources);
				continue;
			}
			try {
				await built.apply();
			} catch (err) {
				onError(err);
				await closeAll(built.resources);
				applied = gen;
				continue;
			}
			const previous = current;
			current = built.resources;
			applied = gen;
			await closeAll(previous);
		}
	}

	function start(): void {
		scheduled = false;
		if (running) return;
		running = loop().finally(() => {
			running = null;
			// A request that raced the loop's last check.
			if (applied < generation) start();
		});
	}

	return {
		request(): void {
			generation++;
			if (scheduled || running) return;
			scheduled = true;
			schedule(start);
		},
		/** Resolves once every request so far is applied (tests). */
		async settled(): Promise<void> {
			while (scheduled || running) {
				if (running) await running;
				else await new Promise<void>((r) => schedule(() => r()));
			}
		},
		get generation(): number {
			return generation;
		},
	};
}

// ─── Accelerator `when` gate ──────────────────────────────────────────────

let lastKeydown: { event: KeyboardEvent; at: number } | null = null;

function recordKeydown(event: KeyboardEvent): void {
	lastKeydown = { event, at: Date.now() };
}

/**
 * The accelerator path of a native-menu item: true when this activation came
 * from pressing the item's key (a matching keydown within the DEC-58 window)
 * and that binding's `when` is false in the live context — the command then
 * does nothing, exactly as its key does through the dispatcher. A click (no
 * matching keydown) is never blocked.
 */
export function acceleratorBlocked(
	commandId: string,
	press: { event: KeyboardEvent; at: number } | null = lastKeydown,
	now: number = Date.now()
): boolean {
	if (!press || now - press.at > DEDUPE_WINDOW_MS) return false;
	const entry = findEntry(commandId, { mac: true });
	if (!entry || !eventMatchesCombo(press.event, entry.key, true)) return false;
	try {
		return !evaluateWhen(entry.when, getContextKeys(press.event.target), getEvalOptions());
	} catch {
		return false;
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

		async function build(): Promise<BuiltMenu> {
			const resources: ClosableResource[] = [];
			const track = <T extends ClosableResource>(r: T): T => {
				resources.push(r);
				return r;
			};

			async function buildEntry(entry: ResolvedMenuEntry) {
				if (entry.kind === 'separator') return track(await PredefinedMenuItem.new({ item: 'Separator' }));
				if (entry.source === 'role') {
					return track(await PredefinedMenuItem.new({ item: predefinedItemOption(entry.predefined) }));
				}
				const id = entry.id;
				const run = menuItemAction(id, () => activateActionId(id));
				return track(
					await MenuItem.new({
						id,
						text: entry.label,
						accelerator: macAccelerator(id),
						action: () => {
							if (acceleratorBlocked(id)) return;
							run();
						},
					})
				);
			}

			async function buildSubmenu(menu: MenuDef) {
				const items = await Promise.all(resolveMenuTree(menu).map(buildEntry));
				return track(await Submenu.new({ text: menu.label, items }));
			}

			try {
				const submenus = await Promise.all(MENU_TREE.map(buildSubmenu));
				const menu = track(await Menu.new({ items: submenus }));
				return { apply: () => menu.setAsAppMenu().then(() => undefined), resources };
			} catch (err) {
				await closeAll(resources);
				throw err;
			}
		}

		window.addEventListener('keydown', recordKeydown, true);

		const queue = createRebuildQueue(build);
		queue.request();
		await queue.settled();
		// G-55 (D-08 state map): the OS draws this menu, so there's no DOM
		// subtree of its own to tag — mark the document root as the closest
		// inspectable "rendered root" for the `native-menu` state, mirroring
		// `native-menu-win`'s `data-state` on the cascade's own root below.
		document.documentElement.dataset.state = 'native-menu';

		// Reordering/hiding an item in `actions.json` (the effective model) or
		// rebinding a key (accelerator labels) must reach the menu bar without
		// a restart (G-ACTIONS §Phase 6 verification — "Menus" section). Both
		// feed the one queue.
		subscribeEffectiveModel(() => queue.request());
		subscribeKeymap(() => queue.request());
	} catch (err) {
		// TODO: native menu wiring requires the Tauri menu plugin + capability.
		// Falling back to JS-side keyboard shortcuts in the workspace.
		// eslint-disable-next-line no-console
		console.warn('[native-menu] could not install, falling back to JS shortcuts', err);
	}
}
