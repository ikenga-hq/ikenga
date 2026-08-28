// Mac-only native menu wiring. Tauri 2 exposes Menu/Submenu/MenuItem from
// `@tauri-apps/api/menu` and lets us set the application menu from JS. If the
// API isn't available (older Tauri version, or capability not granted), we
// log + bail. JS-side keyboard shortcuts in the workspace + command palette
// remain functional regardless.
//
// Menu items dispatch DOM CustomEvents on `window`. Listeners (terminal-eng,
// agent-eng, etc.) subscribe to the event names below.

import { isMac } from '@/lib/platform';
import { isTauri } from '@/lib/transport';

const EVT = {
	newTerminal: 'cmd:new-terminal',
	switchAdapter: 'cmd:switch-adapter',
	openFile: 'cmd:open-file',
	openProjectFolder: 'cmd:open-project-folder',
	toggleSidePane: 'cmd:toggle-side-pane',
	toggleNavRail: 'cmd:toggle-nav-rail',
	openCommandPalette: 'cmd:open-command-palette',
} as const;

function emit(name: string) {
	window.dispatchEvent(new CustomEvent(name));
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

		const fileSubmenu = await Submenu.new({
			text: 'File',
			items: [
				await MenuItem.new({
					id: 'new-session',
					text: 'New Session',
					accelerator: 'CmdOrCtrl+N',
					action: () => emit(EVT.newTerminal),
				}),
				await MenuItem.new({
					id: 'open-file',
					text: 'Open File…',
					accelerator: 'CmdOrCtrl+O',
					action: () => emit(EVT.openFile),
				}),
				await MenuItem.new({
					id: 'open-project',
					text: 'Open Project Folder…',
					accelerator: 'CmdOrCtrl+Shift+O',
					action: () => emit(EVT.openProjectFolder),
				}),
			],
		});

		const editSubmenu = await Submenu.new({
			text: 'Edit',
			items: [
				await PredefinedMenuItem.new({ item: 'Undo' }),
				await PredefinedMenuItem.new({ item: 'Redo' }),
				await PredefinedMenuItem.new({ item: 'Separator' }),
				await PredefinedMenuItem.new({ item: 'Cut' }),
				await PredefinedMenuItem.new({ item: 'Copy' }),
				await PredefinedMenuItem.new({ item: 'Paste' }),
				await PredefinedMenuItem.new({ item: 'SelectAll' }),
			],
		});

		const sessionSubmenu = await Submenu.new({
			text: 'Session',
			items: [
				await MenuItem.new({
					id: 'new-terminal',
					text: 'New Terminal',
					accelerator: 'CmdOrCtrl+T',
					action: () => emit(EVT.newTerminal),
				}),
				await MenuItem.new({
					id: 'switch-adapter',
					text: 'Switch Adapter (coming soon)',
					accelerator: 'CmdOrCtrl+Shift+A',
					action: () => emit(EVT.switchAdapter),
				}),
			],
		});

		const viewSubmenu = await Submenu.new({
			text: 'View',
			items: [
				await MenuItem.new({
					id: 'toggle-side-pane',
					text: 'Toggle Side Pane',
					accelerator: 'CmdOrCtrl+\\',
					action: () => emit(EVT.toggleSidePane),
				}),
				await MenuItem.new({
					id: 'toggle-nav-rail',
					text: 'Toggle Nav Rail',
					accelerator: 'CmdOrCtrl+B',
					action: () => emit(EVT.toggleNavRail),
				}),
				await MenuItem.new({
					id: 'command-palette',
					text: 'Command Palette',
					accelerator: 'CmdOrCtrl+K',
					action: () => emit(EVT.openCommandPalette),
				}),
			],
		});

		const windowSubmenu = await Submenu.new({
			text: 'Window',
			items: [
				await PredefinedMenuItem.new({ item: 'Minimize' }),
				await PredefinedMenuItem.new({ item: 'Maximize' }),
				await PredefinedMenuItem.new({ item: 'Fullscreen' }),
			],
		});

		const helpSubmenu = await Submenu.new({
			text: 'Help',
			items: [
				await MenuItem.new({
					id: 'docs',
					text: 'Docs',
					action: () => {
						window.open('https://royalti.io/docs', '_blank');
					},
				}),
				await MenuItem.new({
					id: 'feedback',
					text: 'Send Feedback',
					action: () => {
						window.open('mailto:feedback@royalti.io?subject=Royalti%20PA%20Feedback', '_blank');
					},
				}),
			],
		});

		const menu = await Menu.new({
			items: [fileSubmenu, editSubmenu, sessionSubmenu, viewSubmenu, windowSubmenu, helpSubmenu],
		});

		await menu.setAsAppMenu();
	} catch (err) {
		// TODO: native menu wiring requires the Tauri menu plugin + capability.
		// Falling back to JS-side keyboard shortcuts in the workspace.
		// eslint-disable-next-line no-console
		console.warn('[native-menu] could not install, falling back to JS shortcuts', err);
	}
}
