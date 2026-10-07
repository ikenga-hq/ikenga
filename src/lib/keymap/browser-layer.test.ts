import { afterEach, describe, expect, it, vi } from 'vitest';
import { buildEffectiveModel } from '@/lib/actions/merge';
import type { ActionsFileState, KeybindingRule } from '@/lib/actions/types';
import {
	browserKeymap,
	browserReservedLeftovers,
	defaultKeymap,
	isBrowserReserved,
} from './browser-layer';
import { DEFAULT_KEYMAP, type KeymapEntry } from './defaults';
import { eventMatchesCombo, formatKeyLabel } from './platform';
import {
	conflicts,
	findEntry,
	getKeymap,
	labelFor,
	resolveKeypress,
	setEffectiveKeymap,
} from './registry';

const session = vi.hoisted(() => ({ browser: false }));
vi.mock('@/lib/transport', async (orig) => ({
	...(await orig<typeof import('@/lib/transport')>()),
	isBrowserSession: () => session.browser,
}));

afterEach(() => {
	session.browser = false;
	setEffectiveKeymap(null);
});

const key = (entries: readonly KeymapEntry[], command: string, mac: boolean) =>
	findEntry(command, { mac, entries })?.key;

describe('isBrowserReserved', () => {
	it.each([
		['mod+w', false, true],
		['mod+w', true, true],
		['mod+shift+t', true, true],
		['ctrl+shift+t', false, true],
		['ctrl+tab', false, true],
		['mod+l', false, true],
		['mod+q', true, true],
		['mod+shift+n', false, true],
		['ctrl+t', true, false], // Ctrl+T is not Cmd+T on macOS
		['alt+w', false, false],
		['alt+shift+t', true, false],
		['mod+b', false, false],
	])('%s (mac=%s) -> %s', (stroke, mac, expected) => {
		expect(isBrowserReserved(stroke, mac)).toBe(expected);
	});
});

describe('browserKeymap', () => {
	const browser = browserKeymap(DEFAULT_KEYMAP);

	it('remaps every browser-reserved default and leaves none behind', () => {
		expect(browserReservedLeftovers(DEFAULT_KEYMAP)).toEqual([]);
		// Sanity: the desktop defaults really do bind reserved chords.
		expect(DEFAULT_KEYMAP.some((e) => e.key === 'mod+w')).toBe(true);
	});

	it('moves close / new-terminal / create commands onto Alt chords', () => {
		expect(key(browser, 'pane.close', false)).toBe('alt+w');
		expect(key(browser, 'tab.close', false)).toBe('alt+shift+w');
		expect(key(browser, 'pane.new-shell-terminal', false)).toBe('alt+t');
		expect(key(browser, 'pane.new-claude-terminal', false)).toBe('alt+shift+t');
		expect(key(browser, 'ngwa.create', false)).toBe('alt+n');
		expect(key(browser, 'pane.new-artifact', false)).toBe('alt+shift+n');
		expect(key(browser, 'people.lock-now', false)).toBe('alt+shift+l');
		expect(key(browser, 'palette.views', true)).toBe('alt+v');
		expect(key(browser, 'pane.reopen', true)).toBe('alt+shift+r');
	});

	it('keeps every non-reserved binding by reference and the same length', () => {
		expect(browser).toHaveLength(DEFAULT_KEYMAP.length);
		expect(browser.find((e) => e.command === 'explorer.toggle')).toBe(
			DEFAULT_KEYMAP.find((e) => e.command === 'explorer.toggle')
		);
	});

	it('introduces no new clash on either platform', () => {
		for (const platform of ['mac', 'other'] as const) {
			const before = conflicts({ platform, entries: DEFAULT_KEYMAP }).clashes.length;
			expect(conflicts({ platform, entries: browser }).clashes).toHaveLength(before);
		}
	});

	it('the remapped chord fires on an Alt keydown (macOS Option arrives via `code`)', () => {
		const ev = {
			key: '†',
			code: 'KeyT',
			altKey: true,
			ctrlKey: false,
			metaKey: false,
			shiftKey: false,
		};
		expect(eventMatchesCombo(ev, 'alt+t', true)).toBe(true);
	});
});

describe('defaultKeymap / getKeymap by session', () => {
	it('is the desktop defaults, untouched, outside a browser', () => {
		session.browser = false;
		expect(defaultKeymap()).toBe(DEFAULT_KEYMAP);
		expect(key(getKeymap(), 'pane.close', false)).toBe('mod+w');
		expect(labelFor('pane.close', { mac: false })).toBe('Ctrl+W');
	});

	it('is the browser layer in a browser, and labels show the effective keys', () => {
		session.browser = true;
		expect(defaultKeymap()).not.toBe(DEFAULT_KEYMAP);
		expect(key(getKeymap(), 'pane.close', false)).toBe('alt+w');
		expect(labelFor('pane.close', { mac: false })).toBe('Alt+W');
		expect(labelFor('pane.close', { mac: true })).toBe('⌥W');
		expect(labelFor('pane.new-shell-terminal', { mac: false })).toBe('Alt+T');
		expect(formatKeyLabel(key(getKeymap(), 'ngwa.create', false) ?? '', { mac: false })).toBe(
			'Alt+N'
		);
	});

	it('the dispatcher resolves Alt+W to pane.close and no longer Ctrl+W', () => {
		session.browser = true;
		const ctx = { inputFocus: false } as never;
		const alt = new KeyboardEvent('keydown', { key: 'w', altKey: true });
		const ctrl = new KeyboardEvent('keydown', { key: 'w', ctrlKey: true });
		expect(resolveKeypress(alt, ctx, 'other').winner?.command).toBe('pane.close');
		expect(resolveKeypress(ctrl, ctx, 'other').winner).toBeNull();
	});
});

describe('effective merge over the browser layer', () => {
	function personal(bindings: KeybindingRule[]) {
		const state = <D>(kind: 'actions' | 'keybindings', document: D | null) =>
			({
				kind,
				scope: 'personal',
				path: `/personal/.ikenga/${kind}.json`,
				present: document != null,
				document,
				stale: false,
				validation: { errors: [], warnings: [] },
				error: null,
			}) as unknown as ActionsFileState<never>;
		return {
			personal: {
				scope: 'personal',
				actions: state('actions', null),
				keybindings: state('keybindings', { version: 1, bindings }),
			},
			project: null,
		} as never;
	}

	it('uses the browser defaults in a browser and the desktop defaults otherwise', () => {
		session.browser = true;
		const b = buildEffectiveModel({ files: null, packages: [] }).keymap.entries;
		expect(key(b, 'pane.close', false)).toBe('alt+w');
		session.browser = false;
		const d = buildEffectiveModel({ files: null, packages: [] }).keymap.entries;
		expect(d).toEqual(DEFAULT_KEYMAP);
	});

	it('a personal rule still overrides a browser default', () => {
		session.browser = true;
		const m = buildEffectiveModel({
			files: personal([{ key: 'mod+shift+e', command: 'pane.close' }]),
			packages: [],
		});
		const closes = m.keymap.entries.filter((e) => e.command === 'pane.close');
		expect(closes.some((e) => e.source === 'personal' && e.key === 'mod+shift+e')).toBe(true);
	});
});
