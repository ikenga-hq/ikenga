// WP-54 — the command table (DEC-56) and the DEC-58 native-menu dedupe.
// Targeted tests, run under the DEC-56 exception.

import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	canRunCommand,
	claimSingleFire,
	DEDUPE_WINDOW_MS,
	defaultCommands,
	getCommandHandler,
	hasCommandHandler,
	menuItemAction,
	ownerOf,
	registerCommand,
	registerCommands,
	registeredCommands,
	resetCommandTableForTests,
	runCommand,
	runEffectiveActionFallback,
	setCommandFallback,
} from './commands';
import { DEFAULT_KEYMAP } from './defaults';
import { KeyDispatcher } from './dispatcher';
import { isHostedCommand } from './registry';

afterEach(() => {
	resetCommandTableForTests();
});

describe('the command table', () => {
	it('runs the registered handler with the invocation', () => {
		const handler = vi.fn();
		registerCommand('pane.close', handler);
		expect(runCommand({ command: 'pane.close', source: 'key' })).toBe(true);
		expect(handler).toHaveBeenCalledWith({ command: 'pane.close', source: 'key' });
	});

	it('is a stack: the latest registration wins; unregistering restores the previous one', () => {
		const first = vi.fn();
		const second = vi.fn();
		const offFirst = registerCommand('palette.open', first);
		const offSecond = registerCommand('palette.open', second);
		runCommand({ command: 'palette.open', source: 'key' });
		expect(second).toHaveBeenCalledTimes(1);
		expect(first).not.toHaveBeenCalled();
		offSecond();
		runCommand({ command: 'palette.open', source: 'key' });
		expect(first).toHaveBeenCalledTimes(1);
		offFirst();
		expect(hasCommandHandler('palette.open')).toBe(false);
	});

	it('unregistering an older registration leaves the newer one in place', () => {
		const older = vi.fn();
		const newer = vi.fn();
		const offOlder = registerCommand('rail.chi', older);
		registerCommand('rail.chi', newer);
		offOlder();
		expect(getCommandHandler('rail.chi')).toBe(newer);
	});

	it('registerCommands registers a set with one unregister', () => {
		const off = registerCommands({ 'zoom.in': vi.fn(), 'zoom.out': vi.fn() });
		expect(registeredCommands()).toEqual(['zoom.in', 'zoom.out']);
		off();
		expect(registeredCommands()).toEqual([]);
	});

	it('a throwing handler is contained and still counts as run', () => {
		const err = vi.spyOn(console, 'error').mockImplementation(() => {});
		registerCommand('pane.close', () => {
			throw new Error('boom');
		});
		expect(runCommand({ command: 'pane.close', source: 'key' })).toBe(true);
		expect(err).toHaveBeenCalled();
		err.mockRestore();
	});

	it('with no handler, the fallback decides', () => {
		setCommandFallback(null);
		expect(runCommand({ command: 'pane.close', source: 'key' })).toBe(false);
		expect(canRunCommand('pane.close')).toBe(false);
		const fallback = vi.fn(() => true);
		setCommandFallback(fallback);
		expect(runCommand({ command: 'release-status', source: 'chord' })).toBe(true);
		expect(fallback).toHaveBeenCalledWith({ command: 'release-status', source: 'chord' });
		expect(canRunCommand('anything')).toBe(true);
	});

	it('the default fallback claims user and package actions, never an unregistered built-in', () => {
		expect(canRunCommand('release-status')).toBe(true);
		expect(canRunCommand('com.ikenga.git:stage-file')).toBe(true);
		expect(canRunCommand('pane.close')).toBe(false);
		expect(canRunCommand('zoom.in')).toBe(false);
		expect(runEffectiveActionFallback({ command: 'pane.close', source: 'key' })).toBe(false);
	});
});

describe('every default command has exactly one owner', () => {
	it('each command of DEFAULT_KEYMAP maps to an owner', () => {
		for (const command of defaultCommands()) {
			expect(ownerOf(command), command).not.toBeNull();
		}
	});

	it('hosted commands belong to their widget; OS commands to lib.rs', () => {
		for (const command of defaultCommands()) {
			const owner = ownerOf(command);
			if (isHostedCommand(command)) expect(['terminal', 'dispatch-input'], command).toContain(owner);
			const os = DEFAULT_KEYMAP.some((e) => e.command === command && e.scope === 'os');
			expect(owner === 'os', command).toBe(os);
		}
	});
});

describe('DEC-58 single fire (native menu ↔ dispatcher)', () => {
	it('macOS: the second path within the window is dropped, whichever comes first', () => {
		expect(claimSingleFire('explorer.toggle', 'dispatcher', { mac: true, now: 1000 })).toBe(true);
		expect(claimSingleFire('explorer.toggle', 'menu', { mac: true, now: 1010 })).toBe(false);

		expect(claimSingleFire('companion.toggle', 'menu', { mac: true, now: 2000 })).toBe(true);
		expect(claimSingleFire('companion.toggle', 'dispatcher', { mac: true, now: 2005 })).toBe(false);
	});

	it('repeats on one path (a held key) always fire; a later press fires again', () => {
		expect(claimSingleFire('zoom.in', 'dispatcher', { mac: true, now: 0 })).toBe(true);
		expect(claimSingleFire('zoom.in', 'dispatcher', { mac: true, now: 30 })).toBe(true);
		expect(claimSingleFire('pane.close', 'dispatcher', { mac: true, now: 0 })).toBe(true);
		expect(claimSingleFire('pane.close', 'menu', { mac: true, now: DEDUPE_WINDOW_MS + 1 })).toBe(true);
	});

	it('only one duplicate is swallowed per press', () => {
		expect(claimSingleFire('tab.close', 'dispatcher', { mac: true, now: 0 })).toBe(true);
		expect(claimSingleFire('tab.close', 'menu', { mac: true, now: 5 })).toBe(false);
		expect(claimSingleFire('tab.close', 'menu', { mac: true, now: 10 })).toBe(true);
	});

	it('off macOS there is no native accelerator, so nothing is deduplicated', () => {
		expect(claimSingleFire('explorer.toggle', 'dispatcher', { mac: false, now: 0 })).toBe(true);
		expect(claimSingleFire('explorer.toggle', 'menu', { mac: false, now: 1 })).toBe(true);
	});

	it('menuItemAction runs the leaf only when the dispatcher has not just run it', () => {
		const leaf = vi.fn();
		const item = menuItemAction('palette.projects', leaf);
		// A pure menu click: runs.
		item();
		expect(leaf).toHaveBeenCalledTimes(1);
		// An item with no command id has no key: never deduplicated.
		const plain = vi.fn();
		menuItemAction(undefined, plain)();
		expect(plain).toHaveBeenCalledTimes(1);
	});

	it('no macOS double-fire: ⌘B through the dispatcher, then its accelerator → one run', () => {
		let now = 5000;
		const runs: string[] = [];
		const dispatcher = new KeyDispatcher({
			platform: () => 'mac',
			getContext: () =>
				({
					inputFocus: false,
					terminalFocus: false,
					explorerFocus: false,
					filesFocus: false,
					paneFocus: false,
					paneKind: undefined,
					resource: undefined,
					resourceExtname: undefined,
					project: undefined,
					sessionFocus: false,
					ngwaItemFocus: false,
					ngwaItemKind: undefined,
					dispatchFocus: false,
					paletteOpen: false,
					permissionCardFocus: false,
					approveGateFocus: false,
					approveGateDetailFocus: false,
					loupeFocus: false,
					pinComposerFocus: false,
					markdownEditorFocus: false,
				}),
			getEvalOptions: () => ({}),
			run: (invocation) => {
				runs.push(`key:${invocation.command}`);
				return true;
			},
			canRun: () => true,
			claim: (command) => claimSingleFire(command, 'dispatcher', { mac: true, now }),
		});
		dispatcher.handleKeydown(new KeyboardEvent('keydown', { key: 'b', metaKey: true, cancelable: true }));
		now += 20;
		if (claimSingleFire('explorer.toggle', 'menu', { mac: true, now })) runs.push('menu:explorer.toggle');
		expect(runs).toEqual(['key:explorer.toggle']);
	});

	it('accelerator first, then the DOM keydown → one run, and the key is still consumed', () => {
		let now = 9000;
		const runs: string[] = [];
		expect(claimSingleFire('pane.split-right', 'menu', { mac: true, now })).toBe(true);
		runs.push('menu:pane.split-right');
		now += 15;
		const dispatcher = new KeyDispatcher({
			platform: () => 'mac',
			getEvalOptions: () => ({}),
			run: (invocation) => {
				runs.push(`key:${invocation.command}`);
				return true;
			},
			canRun: () => true,
			claim: (command) => claimSingleFire(command, 'dispatcher', { mac: true, now }),
		});
		const event = new KeyboardEvent('keydown', { key: '\\', metaKey: true, cancelable: true });
		expect(dispatcher.handleKeydown(event)).toBe(true);
		expect(event.defaultPrevented).toBe(true);
		expect(runs).toEqual(['menu:pane.split-right']);
	});
});
