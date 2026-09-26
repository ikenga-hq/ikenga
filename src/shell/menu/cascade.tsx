// WP-46 — Windows/Linux native-menu parity (D-08 `native-menu-win`).
//
// macOS gets a real OS menu bar (`native-menu.ts`). Windows and Linux get no
// native app menu at all today — the design's fix is a `≡` button at the far
// left of the title row that opens the SAME tree (`./tree.tsx`) as an in-app
// cascading menu, one submenu per top-level entry.
//
// Predefined items (Undo/Redo/…/Minimize/Maximize/Fullscreen/About/Quit) have
// no OS menu to delegate to here, so this file gives each a best-effort
// fallback (`runPredefined`) — `document.execCommand` for text editing (the
// same degraded-but-functional approach most Electron/Tauri apps take absent
// a native edit menu) and `@tauri-apps/api/window` / `plugin-process` for
// window/app lifecycle. "Hide" has no Windows/Linux analogue and is filtered
// out (`macOnly`).
//
// WP-55 (menus render from data): every action leaf reads
// `getEffectiveMenu('native/<top>')` through `useEffectiveModel()` (one hook
// at the top, so this stays inside the rules of hooks despite `MENU_TREE`
// being walked in a `.map()`) — reordering, hiding and package/user appends
// in `actions.json` reach this menu the same way they reach the mac one,
// simply by this being an ordinary React re-render.

import { Menu as MenuIcon } from 'lucide-react';
import {
	DropdownMenu,
	DropdownMenuContent,
	DropdownMenuItem,
	DropdownMenuSeparator,
	DropdownMenuShortcut,
	DropdownMenuSub,
	DropdownMenuSubContent,
	DropdownMenuSubTrigger,
	DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { IconButton } from '@/components/ui/icon-button';
import { useEffectiveModel } from '@/lib/actions/store';
import { isMac } from '@/lib/platform';
import { isTauri } from '@/lib/transport';
import { activateActionId, cascadeKeyLabel, goto, MENU_TREE, resolveMenuTree, type PredefinedKind } from './tree';

const EXEC_COMMAND: Partial<Record<PredefinedKind, string>> = {
	undo: 'undo',
	redo: 'redo',
	cut: 'cut',
	copy: 'copy',
	paste: 'paste',
	selectAll: 'selectAll',
};

/** Best-effort action for a `predefined` leaf that has no `action` of its
 *  own — see the file header for why each branch is what it is. */
async function runPredefined(kind: PredefinedKind): Promise<void> {
	const execCmd = EXEC_COMMAND[kind];
	if (execCmd) {
		// Deprecated but still the pragmatic fallback for a custom in-app menu
		// standing in for the OS Edit menu (no native replacement ships in any
		// browser engine). No-ops harmlessly when nothing editable has focus.
		try {
			document.execCommand(execCmd);
		} catch {
			// ignore — best-effort only
		}
		return;
	}
	if (kind === 'about') {
		goto('/settings/about');
		return;
	}
	if (kind === 'hide') return; // macOnly — never reached, see tree.tsx
	if (!isTauri()) return;
	if (kind === 'quit') {
		const { exit } = await import('@tauri-apps/plugin-process');
		await exit(0);
		return;
	}
	const { getCurrentWindow } = await import('@tauri-apps/api/window');
	const win = getCurrentWindow();
	if (kind === 'minimize') {
		await win.minimize();
	} else if (kind === 'maximize') {
		await win.toggleMaximize();
	} else if (kind === 'fullscreen') {
		const full = await win.isFullscreen();
		await win.setFullscreen(!full);
	}
}

/**
 * The `≡` button + cascading menu — mounted at the far left of the title row
 * on Windows/Linux only. `mac` defaults to the live platform but is
 * overridable so tests (and `title-row.test.tsx`) don't depend on the
 * module-load-time `isMac` constant, matching the `{ mac?: boolean }`
 * override pattern `findEntry`/`conflicts` already use in `registry.ts`.
 */
export function NativeMenuCascade({ mac }: { mac?: boolean } = {}) {
	// Read once at the top (rules of hooks) — `resolveMenuTree` below reads the
	// same started model without its own hook per menu.
	useEffectiveModel();

	if (mac ?? isMac) return null;

	return (
		<div data-testid="native-menu-cascade" data-state="native-menu-win">
			<DropdownMenu>
				<DropdownMenuTrigger asChild>
					<IconButton
						title="Menu"
						aria-label="Application menu"
						aria-haspopup="menu"
						data-testid="native-menu-button"
					>
						<MenuIcon className="h-4 w-4" />
					</IconButton>
				</DropdownMenuTrigger>
				<DropdownMenuContent align="start" side="bottom" className="w-56">
					{MENU_TREE.map((menu) => (
						<DropdownMenuSub key={menu.id}>
							<DropdownMenuSubTrigger>{menu.label}</DropdownMenuSubTrigger>
							<DropdownMenuSubContent className="w-64">
								{resolveMenuTree(menu).map((entry, idx) => {
									if (entry.kind === 'separator') {
										// biome-ignore lint/suspicious/noArrayIndexKey: separators are unkeyed structural markers, stable per menu
										return <DropdownMenuSeparator key={`sep-${idx}`} />;
									}
									if (entry.source === 'role') {
										if (entry.macOnly) return null;
										return (
											<DropdownMenuItem key={entry.id} onSelect={() => void runPredefined(entry.predefined)}>
												{entry.label}
											</DropdownMenuItem>
										);
									}
									const keyLabel = cascadeKeyLabel(entry.id);
									return (
										<DropdownMenuItem
											key={entry.id}
											data-action={entry.id}
											variant={entry.danger ? 'destructive' : undefined}
											onSelect={() => activateActionId(entry.id)}
										>
											{entry.label}
											{keyLabel && <DropdownMenuShortcut>{keyLabel}</DropdownMenuShortcut>}
										</DropdownMenuItem>
									);
								})}
							</DropdownMenuSubContent>
						</DropdownMenuSub>
					))}
				</DropdownMenuContent>
			</DropdownMenu>
		</div>
	);
}
