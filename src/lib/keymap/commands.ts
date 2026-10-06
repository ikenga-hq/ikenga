// The command table (WP-54, DEC-56): command id → the handler that runs it.
//
// The one key dispatcher (`dispatcher.ts`) resolves a keypress to a command
// through the effective keymap; this table says what that command *does*.
// Handlers are registered by the surface that owns the state they act on,
// so a command only runs in a window where its owner is mounted:
//
//   owner                         commands
//   ─────────────────────────────  ─────────────────────────────────────────
//   `installKeyDispatcher()`       zoom.*                  (every window)
//   `Workspace` (workspace.tsx)    pane.*, tab.close, explorer.*,
//                                  companion.toggle, companion.focus-dispatch
//   `useCommandPalette()`          palette.*, shortcuts.*
//   the rail (activity-bar.tsx)    rail.*, ngwa.create
//   the xterm hook                 terminal.*               (hosted, §4.6)
//   the dispatch input             companion.send / new-run / persistent-run
//                                                           (hosted, §4.6)
//   `lib.rs`                       os.*                     (OS-wide, §6)
//   this file (`FRAME_COMMANDS`)   chi.board, people.lock-now
//                                                           (built in, WP-68)
//
// Registration is a stack per command: the latest mounted owner wins and
// unmounting restores the previous one (two palettes in a test, a remount).
// A few frame commands have a built-in handler here (`FRAME_COMMANDS`): it
// sits under the stack, so it runs when no owner registered one, in every
// window, with no component to mount.
// A command with no handler in this window falls back to the effective
// action behind it — a personal / project action runs through the WP-53
// runner, a package action through its fill-only / view run — so a user or
// package key fires without anyone registering it.
//
// DEC-58 single fire: on macOS a native-menu accelerator and the DOM keydown
// can both arrive for one press. Both paths call `claimSingleFire()`; the
// second path to run the same command within `DEDUPE_WINDOW_MS` is dropped,
// whichever arrives first. Repeats on the same path (a held ⌘+) never are.

import { isMacPlatform } from './platform';
import { DEFAULT_KEYMAP } from './defaults';

/** How a command was reached. */
export type CommandSource = 'key' | 'chord' | 'menu' | 'os' | 'palette';

export interface CommandInvocation {
	command: string;
	source: CommandSource;
	/** The keydown that fired it (absent for a chord timeout, a menu click,
	 *  an OS shortcut). */
	event?: KeyboardEvent;
}

export type CommandHandler = (invocation: CommandInvocation) => void;

// ─── Table ─────────────────────────────────────────────────────────────────

const table = new Map<string, CommandHandler[]>();

/** Register `handler` for `command`. Returns an unregister that removes this
 *  exact registration (and restores the previous one, if any). */
export function registerCommand(command: string, handler: CommandHandler): () => void {
	const stack = table.get(command) ?? [];
	stack.push(handler);
	table.set(command, stack);
	return () => {
		const current = table.get(command);
		if (!current) return;
		const i = current.lastIndexOf(handler);
		if (i >= 0) current.splice(i, 1);
		if (current.length === 0) table.delete(command);
	};
}

/** Register several commands at once; one unregister for all of them. */
export function registerCommands(handlers: Readonly<Record<string, CommandHandler>>): () => void {
	const offs = Object.entries(handlers).map(([command, handler]) => registerCommand(command, handler));
	return () => {
		for (const off of offs) off();
	};
}

// ─── Built-in frame commands (WP-68) ──────────────────────────────────────

/**
 * `chi.board` (G-SEATS §8.1): open `/chi`, the seat board, in the focused
 * pane — reusing the tab when it is already open — and land keyboard focus
 * on its selected row. Its `when` (`dispatchFocus`) holds only in the
 * Companion's dispatch input, so in practice it is ⌘2 pressed a second
 * time. Not a Companion-store action.
 */
function openSeatBoardCommand(): void {
	void import('@/shell/chi-board/board-store')
		.then((m) => m.openBoard())
		.catch((err: unknown) => console.warn('[keymap] could not open the seat board:', err));
}

/**
 * `people.lock-now` (D-05 ⌘⇧L, WP-72's Lock now): lock the app now, exactly
 * as Profile › App lock's *Lock now* button does. With no PIN set nothing
 * could unlock it, so — like that button — it does nothing.
 *
 * Desktop only (gap audit rank 29): app lock guards the desktop app, and the
 * headless daemon does not serve `app_lock_lock`, so in a browser the key
 * used to fail silently with a console warning. It is now a no-op there.
 */
function lockNowCommand(): void {
	void (async () => {
		try {
			const [{ appLockLock, isTauri }, { useAppLockStore }] = await Promise.all([
				import('@/lib/tauri-cmd'),
				import('@/shell/people/app-lock-store'),
			]);
			if (!isTauri()) return;
			const current = useAppLockStore.getState().status;
			if (current && (current.locked || !current.secretSet)) return;
			useAppLockStore.getState().setStatus(await appLockLock());
		} catch (err) {
			console.warn('[keymap] Lock now failed:', err);
		}
	})();
}

/** Frame commands whose handler is built in (owner `frame`). Lazy imports
 *  keep the shell out of the dispatcher's module graph and unit tests, as
 *  `runEffectiveActionFallback` does. */
export const FRAME_COMMANDS: Readonly<Record<string, CommandHandler>> = {
	'chi.board': openSeatBoardCommand,
	'people.lock-now': lockNowCommand,
};

/** The handler that runs `command` now: the latest registration, else its
 *  built-in frame handler. */
export function getCommandHandler(command: string): CommandHandler | undefined {
	const stack = table.get(command);
	if (stack) return stack[stack.length - 1];
	return Object.hasOwn(FRAME_COMMANDS, command) ? FRAME_COMMANDS[command] : undefined;
}

export function hasCommandHandler(command: string): boolean {
	return getCommandHandler(command) !== undefined;
}

/** Every command with a registered handler (the Keys tab / iyke can show
 *  which bindings are live in this window). */
export function registeredCommands(): string[] {
	return [...table.keys()].sort();
}

// ─── Fallback: effective actions nobody registered ────────────────────────

/** Runs a command that has no registered handler. Returns true when it will
 *  run (so the dispatcher claims the key), false when nothing can. */
export type CommandFallback = (invocation: CommandInvocation) => boolean;

/** A package action id (`${pkg_id}:${id}`, G-ACTIONS §10.1). */
function isPackageId(id: string): boolean {
	return id.includes(':');
}

/** A user action id: no `.`, no `:` (§10.1). Grandfathered bare built-ins
 *  (`open`, `rename`, …) share the shape; the effective model tells them
 *  apart at run time (a built-in has no `userAction`). */
function isUserShapedId(id: string): boolean {
	return !id.includes('.') && !isPackageId(id);
}

/**
 * Default fallback: a personal / project action runs through the WP-53
 * runner (trust gate included); a package action runs its own `view` /
 * fill-only `dispatch` run. Lazy imports keep the store and runner out of
 * the dispatcher's module graph (and out of its unit tests).
 */
export const runEffectiveActionFallback: CommandFallback = (invocation) => {
	const id = invocation.command;
	if (!isPackageId(id) && !isUserShapedId(id)) return false;
	void (async () => {
		try {
			const { getEffectiveAction } = await import('@/lib/actions/store');
			const action = getEffectiveAction(id);
			if (!action) return;
			if ((action.source === 'personal' || action.source === 'project') && action.userAction) {
				const { runAction } = await import('@/lib/actions/runner');
				await runAction({ id: action.id, name: action.name, run: action.userAction.run, scope: action.source });
				return;
			}
			if (action.source === 'package') {
				const run = action.run;
				if (run.kind === 'view') {
					const { usePaneStore } = await import('@/lib/panes/pane-store');
					usePaneStore.getState().navigateFocused(run.route);
				} else if (run.kind === 'dispatch') {
					// Fill-only (DEC-63.3): pre-fill the dispatch input, never send.
					const { handToChi } = await import('@/shell/companion/companion-store');
					handToChi(run.prompt);
				}
			}
		} catch (err) {
			console.warn(`[keymap] could not run action "${id}":`, err);
		}
	})();
	return true;
};

let fallback: CommandFallback | null = runEffectiveActionFallback;

/** Replace the fallback (`null` = none). Tests use this; the app keeps the
 *  default. */
export function setCommandFallback(next: CommandFallback | null): void {
	fallback = next;
}

/**
 * Run `command`: its registered handler, else the fallback. Returns true
 * when something ran (or will run), false when this window cannot run it —
 * the dispatcher then leaves the keystroke alone (no `preventDefault`).
 */
export function runCommand(invocation: CommandInvocation): boolean {
	const handler = getCommandHandler(invocation.command);
	if (handler) {
		try {
			handler(invocation);
		} catch (err) {
			console.error(`[keymap] command "${invocation.command}" failed:`, err);
		}
		return true;
	}
	return fallback ? fallback(invocation) : false;
}

/** Would `runCommand` run something for `command` in this window? Pure. */
export function canRunCommand(command: string): boolean {
	if (hasCommandHandler(command)) return true;
	if (!fallback) return false;
	if (fallback !== runEffectiveActionFallback) return true;
	return isPackageId(command) || isUserShapedId(command);
}

// ─── DEC-58 single fire (native menu ↔ dispatcher) ────────────────────────

/** A press reaches a command on at most one path. */
export type FirePath = 'dispatcher' | 'menu';

/** Long enough to cover the IPC hop of a native-menu event; short enough
 *  that a deliberate second press is never swallowed. */
export const DEDUPE_WINDOW_MS = 300;

const lastFire = new Map<string, { path: FirePath; at: number }>();

/**
 * Claim `command` for one press on `path`. False when the *other* path ran
 * the same command within `DEDUPE_WINDOW_MS` — the caller must then not run
 * it. Only macOS has a native menu with accelerators; elsewhere every claim
 * succeeds (the in-app `≡` cascade has no accelerators).
 */
export function claimSingleFire(
	command: string,
	path: FirePath,
	opts?: { now?: number; mac?: boolean }
): boolean {
	const mac = opts?.mac ?? isMacPlatform();
	if (!mac) return true;
	const now = opts?.now ?? Date.now();
	const last = lastFire.get(command);
	if (last && last.path !== path && now - last.at < DEDUPE_WINDOW_MS) {
		// The duplicate is consumed: a third arrival starts a new press.
		lastFire.delete(command);
		return false;
	}
	lastFire.set(command, { path, at: now });
	return true;
}

/** Wrap a native-menu item action so it fires once per press (DEC-58). An
 *  item without a command id has no key, so nothing to deduplicate. */
export function menuItemAction(commandId: string | undefined, action: () => void): () => void {
	if (!commandId) return action;
	return () => {
		if (!claimSingleFire(commandId, 'menu')) return;
		action();
	};
}

// ─── Coverage ─────────────────────────────────────────────────────────────

/** Who fires each default command (see the header). `commands.test.ts`
 *  proves every default command has exactly one owner. */
export type CommandOwner =
	| 'window'
	| 'workspace'
	| 'palette'
	| 'rail'
	| 'terminal'
	| 'dispatch-input'
	| 'native-menu'
	| 'os'
	| 'widget'
	| 'frame';

export function ownerOf(command: string): CommandOwner | null {
	// WP-68: built-in handlers in this file (`FRAME_COMMANDS`).
	if (Object.hasOwn(FRAME_COMMANDS, command)) return 'frame';
	if (command.startsWith('zoom.')) return 'window';
	if (command.startsWith('terminal.')) return 'terminal';
	if (command.startsWith('os.')) return 'os';
	if (command.startsWith('menu.')) return 'native-menu';
	if (command.startsWith('rail.') || command === 'ngwa.create') return 'rail';
	if (command.startsWith('palette.') || command.startsWith('shortcuts.')) return 'palette';
	if (command === 'companion.send' || command === 'companion.new-run' || command === 'companion.persistent-run') {
		return 'dispatch-input';
	}
	if (
		command.startsWith('pane.') ||
		command.startsWith('explorer.') ||
		command === 'tab.close' ||
		command === 'companion.toggle' ||
		command === 'companion.focus-dispatch'
	) {
		return 'workspace';
	}
	// WP-56: widget-local commands registered by the surface that owns the
	// focused element (permission card, approve gate, Studio loupe/pin
	// composer, markdown editor) — never hosted, so never in §4.6's table.
	if (
		command.startsWith('companion.permission-') ||
		command.startsWith('approve-gate.') ||
		command.startsWith('studio.') ||
		command.startsWith('markdown.')
	) {
		return 'widget';
	}
	return null;
}

/** The distinct commands of the default keymap. */
export function defaultCommands(): string[] {
	return [...new Set(DEFAULT_KEYMAP.map((e) => e.command))];
}

/** Test seam: forget every registration and the single-fire history. */
export function resetCommandTableForTests(): void {
	table.clear();
	lastFire.clear();
	fallback = runEffectiveActionFallback;
}
