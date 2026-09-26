// WP-54 — the one key dispatcher (DEC-56). Targeted tests, run under the
// DEC-56 exception. Against `01` §Phase 6 verification (Keys) and the WP-54
// definition of done: a personal rebind changes what fires (per command
// group), a hidden built-in still fires, the three DEC-64 re-keys, ⌘.
// (A-6), the Companion Enter keys (A-2), the `os.*` rows and their re-sync,
// the typing guard, IME, and ⌘K's chord mode (DEC-57).

import { describe, expect, it } from 'vitest';
import { buildEffectiveModel } from '@/lib/actions/merge';
import type {
	ActionsDocument,
	ActionsFilesResult,
	ActionsFileState,
	ActionsScope,
	KeybindingRule,
	KeybindingsDocument,
} from '@/lib/actions/types';
import type { ContextKeys } from './context-keys';
import { DEFAULT_KEYMAP, type KeymapEntry } from './defaults';
import { KeyDispatcher, type OsShortcutRule, type OsShortcutStatus, osRulesFor, startOsShortcutSync } from './dispatcher';
import type { KeymapPlatform } from './registry';

// ─── Fixtures ────────────────────────────────────────────────────────────────

function fileState<D extends ActionsDocument | KeybindingsDocument>(
	kind: 'actions' | 'keybindings',
	scope: ActionsScope,
	document: D | null
): ActionsFileState<D> {
	return {
		kind,
		scope,
		path: `/${scope}/.ikenga/${kind}.json`,
		present: document != null,
		document,
		stale: false,
		validation: { errors: [], warnings: [] },
		error: null,
	};
}

/** A personal `~/.ikenga/{actions,keybindings}.json` pair. */
function personalFiles(bindings: KeybindingRule[], actions?: ActionsDocument): ActionsFilesResult {
	return {
		personal: {
			scope: 'personal',
			actions: fileState('actions', 'personal', actions ?? null),
			keybindings: fileState('keybindings', 'personal', { version: 1, bindings }),
		},
		project: {
			scope: 'project',
			actions: fileState<ActionsDocument>('actions', 'project', null),
			keybindings: fileState<KeybindingsDocument>('keybindings', 'project', null),
		},
		projectId: 'p1',
		projectRoot: '/work/p1',
		projectKeybindingsTrust: { hash: null, ruleCount: 0, state: 'absent' },
		trustError: null,
	};
}

/** The effective keymap with personal `keybindings.json` rules merged. */
function effectiveWith(bindings: KeybindingRule[], actions?: ActionsDocument): KeymapEntry[] {
	return buildEffectiveModel({ files: personalFiles(bindings, actions), packages: [] }).keymap.entries;
}

function ctx(partial: Partial<ContextKeys> = {}): ContextKeys {
	return {
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
		// WP-56 (B-21 additive rule, G-ACTIONS §10.2 "Reserved for WP-56").
		permissionCardFocus: false,
		approveGateFocus: false,
		loupeFocus: false,
		pinComposerFocus: false,
		markdownEditorFocus: false,
		...partial,
	};
}

interface Harness {
	dispatcher: KeyDispatcher;
	fired: string[];
	timers: Array<(() => void) | null>;
	setContext(partial: Partial<ContextKeys>): void;
	press(init: KeyboardEventInit): { event: KeyboardEvent; handled: boolean };
	flushTimers(): void;
}

function harness(opts: {
	platform: KeymapPlatform;
	entries?: readonly KeymapEntry[];
	context?: Partial<ContextKeys>;
	canRun?: (command: string) => boolean;
}): Harness {
	const entries = opts.entries ?? DEFAULT_KEYMAP;
	let current = ctx(opts.context);
	const fired: string[] = [];
	const timers: Array<(() => void) | null> = [];
	const dispatcher = new KeyDispatcher({
		getEntries: () => entries,
		platform: () => opts.platform,
		getContext: () => current,
		getEvalOptions: () => ({}),
		run: (invocation) => {
			fired.push(invocation.command);
			return true;
		},
		canRun: opts.canRun ?? (() => true),
		claim: () => true,
		setTimer: (fn) => {
			timers.push(fn);
			return timers.length - 1;
		},
		clearTimer: (handle) => {
			timers[handle as number] = null;
		},
	});
	return {
		dispatcher,
		fired,
		timers,
		setContext(partial) {
			current = ctx(partial);
		},
		press(init) {
			const event = new KeyboardEvent('keydown', { cancelable: true, bubbles: true, ...init });
			const handled = dispatcher.handleKeydown(event);
			return { event, handled };
		},
		flushTimers() {
			for (let i = 0; i < timers.length; i++) {
				const fn = timers[i];
				timers[i] = null;
				fn?.();
			}
		},
	};
}

/** `mod+<key>` as an event on `platform`. */
function mod(platform: KeymapPlatform, key: string, extra: KeyboardEventInit = {}): KeyboardEventInit {
	return platform === 'mac' ? { key, metaKey: true, ...extra } : { key, ctrlKey: true, ...extra };
}

const PLATFORMS: KeymapPlatform[] = ['mac', 'other'];

// ─── Rebinding changes what fires (one test per command group) ──────────────

interface RebindCase {
	group: string;
	command: string;
	/** Default key (registry grammar) and the event that presses it. */
	oldKey: string;
	oldPress: (p: KeymapPlatform) => KeyboardEventInit;
	/** New personal key and the event that presses it. */
	newKey: string;
	newPress: (p: KeymapPlatform) => KeyboardEventInit;
	platforms?: KeymapPlatform[];
	context?: Partial<ContextKeys>;
	/** The personal rule's `when` (default: none = always). */
	when?: string;
}

const REBINDS: RebindCase[] = [
	{
		group: 'rail',
		command: 'rail.chi',
		oldKey: 'mod+2',
		oldPress: (p) => mod(p, '2'),
		newKey: 'mod+alt+shift+2',
		newPress: (p) => mod(p, '@', { altKey: true, shiftKey: true, code: 'Digit2' }),
	},
	{
		group: 'palette',
		command: 'palette.projects',
		oldKey: 'mod+p',
		oldPress: (p) => mod(p, 'p'),
		newKey: 'mod+alt+p',
		newPress: (p) => mod(p, 'p', { altKey: true }),
	},
	{
		group: 'shortcuts',
		command: 'shortcuts.open',
		oldKey: 'mod+/',
		oldPress: (p) => mod(p, '/'),
		newKey: 'f1',
		newPress: () => ({ key: 'F1' }),
		when: '!inputFocus && !paletteOpen',
	},
	{
		group: 'pane',
		command: 'pane.close',
		oldKey: 'mod+w',
		oldPress: (p) => mod(p, 'w'),
		newKey: 'mod+alt+w',
		newPress: (p) => mod(p, 'w', { altKey: true }),
	},
	{
		group: 'pane focus (DEC-64 keys)',
		command: 'pane.focus-2',
		oldKey: 'alt+2',
		oldPress: () => ({ key: '2', altKey: true }),
		newKey: 'ctrl+alt+2',
		newPress: () => ({ key: '2', ctrlKey: true, altKey: true }),
		platforms: ['other'],
	},
	{
		group: 'tab',
		command: 'tab.close',
		oldKey: 'mod+shift+w',
		oldPress: (p) => mod(p, 'W', { shiftKey: true }),
		newKey: 'mod+alt+shift+w',
		newPress: (p) => mod(p, 'W', { shiftKey: true, altKey: true }),
	},
	{
		group: 'explorer',
		command: 'explorer.toggle',
		oldKey: 'mod+b',
		oldPress: (p) => mod(p, 'b'),
		newKey: 'mod+shift+y',
		newPress: (p) => mod(p, 'Y', { shiftKey: true }),
	},
	{
		group: 'companion',
		command: 'companion.toggle',
		oldKey: 'mod+j',
		oldPress: (p) => mod(p, 'j'),
		newKey: 'mod+alt+j',
		newPress: (p) => mod(p, 'j', { altKey: true }),
	},
	{
		group: 'ngwa',
		command: 'ngwa.create',
		oldKey: 'mod+n',
		oldPress: (p) => mod(p, 'n'),
		newKey: 'mod+alt+n',
		newPress: (p) => mod(p, 'n', { altKey: true }),
	},
	{
		group: 'zoom',
		command: 'zoom.reset',
		oldKey: 'mod+0',
		oldPress: (p) => mod(p, '0'),
		newKey: 'mod+alt+0',
		newPress: (p) => mod(p, '0', { altKey: true }),
	},
];

describe('a personal `keybindings.json` rebind changes what fires', () => {
	for (const c of REBINDS) {
		for (const platform of c.platforms ?? PLATFORMS) {
			it(`${c.group}: ${c.command} ${c.oldKey} → ${c.newKey} (${platform})`, () => {
				const before = harness({ platform, context: c.context });
				before.press(c.oldPress(platform));
				expect(before.fired).toEqual([c.command]);

				const entries = effectiveWith([
					{ key: c.oldKey, command: `-${c.command}` },
					{ key: c.newKey, command: c.command, ...(c.when ? { when: c.when } : {}) },
				]);
				const after = harness({ platform, entries, context: c.context });
				after.press(c.oldPress(platform));
				expect(after.fired).toEqual([]);
				after.press(c.newPress(platform));
				expect(after.fired).toEqual([c.command]);
			});
		}
	}

	it('an added personal binding keeps the default one too (VS Code semantics)', () => {
		const entries = effectiveWith([{ key: 'mod+shift+y', command: 'explorer.toggle' }]);
		const h = harness({ platform: 'mac', entries });
		h.press(mod('mac', 'b'));
		h.press(mod('mac', 'Y', { shiftKey: true }));
		expect(h.fired).toEqual(['explorer.toggle', 'explorer.toggle']);
	});

	it('a personal rule on a default key wins over the default (layer, §2.3)', () => {
		const entries = effectiveWith([{ key: 'mod+b', command: 'companion.toggle' }]);
		const h = harness({ platform: 'other', entries });
		h.press(mod('other', 'b'));
		expect(h.fired).toEqual(['companion.toggle']);
	});
});

// ─── Hiding is not unbinding (DEC-58) ────────────────────────────────────────

describe('a hidden built-in still fires (DEC-58)', () => {
	it('explorer.toggle hidden from the native View menu keeps ⌘B', () => {
		const model = buildEffectiveModel({
			files: personalFiles([], { version: 1, menus: { 'native/view': { hidden: ['explorer.toggle'] } } }),
			packages: [],
		});
		const view = model.menus.get('native/view');
		expect(view?.hidden).toContain('explorer.toggle');
		expect(view?.items.some((i) => i.kind === 'action' && i.id === 'explorer.toggle')).toBe(false);
		for (const platform of PLATFORMS) {
			const h = harness({ platform, entries: model.keymap.entries });
			h.press(mod(platform, 'b'));
			expect(h.fired).toEqual(['explorer.toggle']);
		}
	});
});

// ─── DEC-64: the three shipped double-fires are gone ─────────────────────────

describe('DEC-64 re-keys (G-ACTIONS §2.4)', () => {
	it('Windows/Linux: Alt+1–6 focuses pane N; Ctrl+1–3 switch the rail and nothing else', () => {
		const h = harness({ platform: 'other' });
		for (let n = 1; n <= 6; n++) h.press({ key: String(n), altKey: true });
		expect(h.fired).toEqual([1, 2, 3, 4, 5, 6].map((n) => `pane.focus-${n}`));

		h.fired.length = 0;
		h.press({ key: '1', ctrlKey: true });
		h.press({ key: '2', ctrlKey: true });
		h.press({ key: '3', ctrlKey: true });
		expect(h.fired).toEqual(['rail.project', 'rail.chi', 'rail.ngwa']);

		// Ctrl+4–6 is nobody's any more on Windows/Linux (⌘4–6 are retired).
		h.fired.length = 0;
		for (const n of [4, 5, 6]) h.press({ key: String(n), ctrlKey: true });
		expect(h.fired).toEqual([]);
	});

	it('Windows/Linux: Ctrl+1 inside a text field fires no pane focus (it used to, `always`)', () => {
		const h = harness({ platform: 'other', context: { inputFocus: true } });
		h.press({ key: '1', ctrlKey: true });
		expect(h.fired).toEqual([]);
		h.press({ key: '1', altKey: true });
		expect(h.fired).toEqual(['pane.focus-1']);
	});

	it('macOS: ⌃1–⌃6 focus pane N; ⌘1–⌘3 switch the rail', () => {
		const h = harness({ platform: 'mac' });
		for (let n = 1; n <= 6; n++) h.press({ key: String(n), ctrlKey: true });
		expect(h.fired).toEqual([1, 2, 3, 4, 5, 6].map((n) => `pane.focus-${n}`));
		h.fired.length = 0;
		h.press(mod('mac', '1'));
		expect(h.fired).toEqual(['rail.project']);
		// Alt+1 is not a mac binding.
		h.fired.length = 0;
		h.press({ key: '1', altKey: true });
		expect(h.fired).toEqual([]);
	});

	it('macOS: ⌘N fires only ngwa.create', () => {
		const h = harness({ platform: 'mac' });
		h.press(mod('mac', 'n'));
		expect(h.fired).toEqual(['ngwa.create']);
		expect(DEFAULT_KEYMAP.some((e) => e.command === 'menu.new-session')).toBe(false);
	});

	it('macOS: ⌘T fires only palette.views', () => {
		const h = harness({ platform: 'mac' });
		h.press(mod('mac', 't'));
		expect(h.fired).toEqual(['palette.views']);
		expect(DEFAULT_KEYMAP.some((e) => e.command === 'menu.new-terminal')).toBe(false);
	});

	it('no default key is shared by two commands among the former pairs', () => {
		for (const command of ['menu.new-session', 'menu.new-terminal', 'session.switch-adapter']) {
			expect(DEFAULT_KEYMAP.some((e) => e.command === command), command).toBe(false);
		}
		expect('knownOverlap' in (DEFAULT_KEYMAP[0] as object)).toBe(false);
	});
});

// ─── Missing defaults restored (§10.2) ───────────────────────────────────────

describe('⌘. toggles hidden files in the Explorer (A-6)', () => {
	for (const platform of PLATFORMS) {
		it(`fires explorer.toggle-hidden only with Explorer focus (${platform})`, () => {
			const h = harness({ platform });
			h.press(mod(platform, '.'));
			expect(h.fired).toEqual([]);
			h.setContext({ explorerFocus: true });
			h.press(mod(platform, '.'));
			expect(h.fired).toEqual(['explorer.toggle-hidden']);
		});
	}
});

describe('the other §10.2 additions', () => {
	it('⌘⇧A focuses the dispatch input; ⌘⌥←/→ switch tabs in a pane; ⌘⌥↑/↓ move pane focus', () => {
		const h = harness({ platform: 'mac', context: { paneFocus: true } });
		h.press(mod('mac', 'A', { shiftKey: true }));
		h.press(mod('mac', 'ArrowLeft', { altKey: true }));
		h.press(mod('mac', 'ArrowRight', { altKey: true }));
		h.press(mod('mac', 'ArrowUp', { altKey: true }));
		h.press(mod('mac', 'ArrowDown', { altKey: true }));
		expect(h.fired).toEqual([
			'companion.focus-dispatch',
			'pane.tab-prev',
			'pane.tab-next',
			'pane.focus-up',
			'pane.focus-down',
		]);
	});

	it('zoom works while typing (`always`), including ⌘+ / numpad + and the restored ⌘⇧- (Round 42 hand-off)', () => {
		const h = harness({ platform: 'other', context: { inputFocus: true } });
		h.press(mod('other', '='));
		h.press(mod('other', '+', { shiftKey: true, code: 'Equal' }));
		h.press(mod('other', '-'));
		h.press(mod('other', '_', { shiftKey: true, code: 'Minus' }));
		h.press(mod('other', '0'));
		expect(h.fired).toEqual(['zoom.in', 'zoom.in', 'zoom.out', 'zoom.out', 'zoom.reset']);
	});
});

// ─── Companion Enter keys: hosted by the dispatch input (A-2, §4.6) ──────────

describe('Companion dispatch keys (hosted, `dispatchFocus`)', () => {
	it('Enter outside the dispatch input fires no Companion command', () => {
		const h = harness({ platform: 'mac' });
		for (const init of [{ key: 'Enter' }, { key: 'Enter', shiftKey: true }, { key: 'Enter', altKey: true }]) {
			const { handled, event } = h.press(init);
			expect(handled).toBe(false);
			expect(event.defaultPrevented).toBe(false);
			const hosted = h.dispatcher.hostedWinner(event, ctx(), 'dispatch');
			expect(hosted).toBeNull();
		}
		expect(h.fired).toEqual([]);
	});

	it('inside the dispatch input the owner gets the command; the frame dispatcher stands back', () => {
		const h = harness({ platform: 'other', context: { dispatchFocus: true, inputFocus: true } });
		const cases: Array<[KeyboardEventInit, string]> = [
			[{ key: 'Enter' }, 'companion.send'],
			[{ key: 'Enter', shiftKey: true }, 'companion.new-run'],
			[{ key: 'Enter', altKey: true }, 'companion.persistent-run'],
		];
		for (const [init, command] of cases) {
			const event = new KeyboardEvent('keydown', { cancelable: true, ...init });
			expect(h.dispatcher.hostedWinner(event, ctx({ dispatchFocus: true }), 'dispatch')?.command).toBe(command);
			expect(h.dispatcher.handleKeydown(event)).toBe(false);
			expect(event.defaultPrevented).toBe(false);
		}
		expect(h.fired).toEqual([]);
	});

	it('rebinding companion.send changes the dispatch input’s send key', () => {
		const entries = effectiveWith([
			{ key: 'enter', command: '-companion.send' },
			{ key: 'mod+enter', command: 'companion.send', when: 'dispatchFocus' },
		]);
		const h = harness({ platform: 'mac', entries });
		const inInput = ctx({ dispatchFocus: true, inputFocus: true });
		const enter = new KeyboardEvent('keydown', { key: 'Enter' });
		expect(h.dispatcher.hostedWinner(enter, inInput, 'dispatch')).toBeNull();
		const cmdEnter = new KeyboardEvent('keydown', { key: 'Enter', metaKey: true });
		expect(h.dispatcher.hostedWinner(cmdEnter, inInput, 'dispatch')?.command).toBe('companion.send');
		// ⇧Enter is untouched.
		const shiftEnter = new KeyboardEvent('keydown', { key: 'Enter', shiftKey: true });
		expect(h.dispatcher.hostedWinner(shiftEnter, inInput, 'dispatch')?.command).toBe('companion.new-run');
	});
});

// ─── The terminal keeps its keys (hosted `terminal.*`) ───────────────────────

describe('terminal ownership (DEC-56, DEC-57)', () => {
	it('macOS ⌘K in the terminal is terminal.clear’s: the dispatcher neither fires nor prevents it', () => {
		const h = harness({ platform: 'mac', context: { terminalFocus: true, inputFocus: true } });
		const { handled, event } = h.press(mod('mac', 'k'));
		expect(handled).toBe(false);
		expect(event.defaultPrevented).toBe(false);
		expect(h.fired).toEqual([]);
		expect(h.dispatcher.peek(event).claimed).toBe(false);
	});

	it('an `always` frame key (zoom) is claimed from inside the terminal', () => {
		const h = harness({ platform: 'other', context: { terminalFocus: true, inputFocus: true } });
		const event = new KeyboardEvent('keydown', mod('other', '='));
		expect(h.dispatcher.peek(event)).toMatchObject({ claimed: true, chord: false });
		expect(h.dispatcher.peek(event).winner?.command).toBe('zoom.in');
	});

	it('a `!inputFocus` frame key is not claimed from inside the terminal', () => {
		const h = harness({ platform: 'other', context: { terminalFocus: true, inputFocus: true } });
		expect(h.dispatcher.peek(new KeyboardEvent('keydown', mod('other', 'w'))).claimed).toBe(false);
	});
});

// ─── Typing guard and keyboard robustness (§3.3) ─────────────────────────────

describe('nothing fires while typing unless its `when` says so', () => {
	it('`!inputFocus` rules stay quiet in a text field; `always` rules fire', () => {
		const h = harness({ platform: 'mac', context: { inputFocus: true } });
		for (const key of ['w', 'k', 'p', 'j', '1', 'n', 't']) h.press(mod('mac', key));
		h.press({ key: '?', shiftKey: true });
		expect(h.fired).toEqual([]);
		h.press(mod('mac', 'b'));
		h.press(mod('mac', '\\'));
		expect(h.fired).toEqual(['explorer.toggle', 'pane.split-right']);
	});

	it('IME composition and Dead keys never fire', () => {
		const h = harness({ platform: 'mac' });
		const composing = h.press({ key: 'b', metaKey: true, isComposing: true });
		expect(composing.handled).toBe(false);
		const ime = h.press({ key: 'Process', metaKey: true });
		expect(ime.handled).toBe(false);
		const dead = h.press({ key: 'Dead', metaKey: true });
		expect(dead.handled).toBe(false);
		expect(h.fired).toEqual([]);
	});

	it('an event a widget already consumed is left alone', () => {
		const h = harness({ platform: 'mac' });
		const event = new KeyboardEvent('keydown', { ...mod('mac', 'b'), cancelable: true });
		event.preventDefault();
		expect(h.dispatcher.handleKeydown(event)).toBe(false);
		expect(h.fired).toEqual([]);
	});

	it('a command this window cannot run keeps the key’s default (no preventDefault)', () => {
		const h = harness({ platform: 'mac', canRun: (c) => c !== 'pane.close' });
		const { handled, event } = h.press(mod('mac', 'w'));
		expect(handled).toBe(false);
		expect(event.defaultPrevented).toBe(false);
		expect(h.fired).toEqual([]);
	});

	it('a fired command prevents the default', () => {
		const h = harness({ platform: 'mac' });
		const { handled, event } = h.press(mod('mac', 'b'));
		expect(handled).toBe(true);
		expect(event.defaultPrevented).toBe(true);
	});
});

// ─── ⌘K: palette open / close, chord mode (DEC-57) ───────────────────────────

describe('⌘K and chord mode (DEC-57, §3.2)', () => {
	it('with no ⌘K chord bound, ⌘K opens the palette at once (no timer)', () => {
		for (const platform of PLATFORMS) {
			const h = harness({ platform });
			h.press(mod(platform, 'k'));
			expect(h.fired).toEqual(['palette.open']);
			expect(h.timers).toEqual([]);
			expect(h.dispatcher.chordPending).toBe(false);
		}
	});

	it('with the palette open, ⌘K closes it — even from its own search input', () => {
		const h = harness({ platform: 'other', context: { paletteOpen: true, inputFocus: true } });
		h.press(mod('other', 'k'));
		expect(h.fired).toEqual(['palette.close']);
		h.fired.length = 0;
		h.press(mod('other', '/'));
		expect(h.fired).toEqual(['palette.toggle-shortcuts']);
	});

	const CHORD = [{ key: 'mod+k mod+r', command: 'release-status' }];

	it('with a ⌘K chord bound, ⌘K waits; the second stroke fires the chord', () => {
		const h = harness({ platform: 'mac', entries: effectiveWith(CHORD) });
		const first = h.press(mod('mac', 'k'));
		expect(first.handled).toBe(true);
		expect(first.event.defaultPrevented).toBe(true);
		expect(h.fired).toEqual([]);
		expect(h.dispatcher.chordPending).toBe(true);
		h.press(mod('mac', 'r'));
		expect(h.fired).toEqual(['release-status']);
		expect(h.dispatcher.chordPending).toBe(false);
	});

	it('900 ms with no second stroke: the palette opens', () => {
		const h = harness({ platform: 'mac', entries: effectiveWith(CHORD) });
		h.press(mod('mac', 'k'));
		expect(h.fired).toEqual([]);
		h.flushTimers();
		expect(h.fired).toEqual(['palette.open']);
	});

	it('a second stroke that completes no chord: the palette opens, then that stroke fires', () => {
		const h = harness({ platform: 'mac', entries: effectiveWith(CHORD) });
		h.press(mod('mac', 'k'));
		h.press(mod('mac', 'b'));
		expect(h.fired).toEqual(['palette.open', 'explorer.toggle']);
	});

	it('Escape ends chord mode and fires nothing', () => {
		const h = harness({ platform: 'mac', entries: effectiveWith(CHORD) });
		h.press(mod('mac', 'k'));
		const esc = h.press({ key: 'Escape' });
		expect(esc.handled).toBe(true);
		expect(h.fired).toEqual([]);
		h.flushTimers();
		expect(h.fired).toEqual([]);
	});

	it('owner carve-out: in the terminal ⌘K is not held back by a chord whose `when` is false', () => {
		const h = harness({
			platform: 'mac',
			entries: effectiveWith([{ key: 'mod+k mod+r', command: 'release-status', when: 'explorerFocus' }]),
			context: { terminalFocus: true, inputFocus: true },
		});
		const { handled } = h.press(mod('mac', 'k'));
		expect(handled).toBe(false);
		expect(h.dispatcher.chordPending).toBe(false);
	});
});

// ─── OS-wide rules (DEC-60, §6) ──────────────────────────────────────────────

describe('`os.*` rows and their OS registration', () => {
	it('the three `os.*` rows exist as `scope: os` defaults', () => {
		const os = DEFAULT_KEYMAP.filter((e) => e.scope === 'os');
		expect([...new Set(os.map((e) => e.command))].sort()).toEqual([
			'os.screenshot-pane',
			'os.screenshot-window',
			'os.summon',
		]);
		expect(osRulesFor(DEFAULT_KEYMAP, 'mac')).toEqual([
			{ command: 'os.summon', key: 'alt+space' },
			{ command: 'os.screenshot-window', key: 'ctrl+alt+shift+s' },
			{ command: 'os.screenshot-pane', key: 'ctrl+alt+shift+p' },
		]);
		expect(osRulesFor(DEFAULT_KEYMAP, 'other')[0]).toEqual({ command: 'os.summon', key: 'meta+space' });
	});

	it('OS rules are never dispatched in-app', () => {
		const h = harness({ platform: 'mac' });
		h.press({ key: 'S', ctrlKey: true, altKey: true, shiftKey: true, code: 'KeyS' });
		h.press({ key: ' ', altKey: true, code: 'Space' });
		expect(h.fired).toEqual([]);
	});

	it('a personal rebind re-registers with the OS; an unchanged publish does not', async () => {
		let entries: readonly KeymapEntry[] = DEFAULT_KEYMAP;
		let publish: () => void = () => {};
		const applied: OsShortcutRule[][] = [];
		const stop = startOsShortcutSync({
			apply: (rules) => {
				applied.push(rules);
				return Promise.resolve(
					rules.map((r): OsShortcutStatus => ({ ...r, registered: true, reason: null }))
				);
			},
			getEntries: () => entries,
			platform: () => 'mac',
			subscribe: (listener) => {
				publish = listener;
				return () => {};
			},
			listenOsCommand: () => Promise.resolve(() => {}),
		});
		expect(applied).toHaveLength(1);
		expect(applied[0].find((r) => r.command === 'os.summon')?.key).toBe('alt+space');

		publish();
		expect(applied).toHaveLength(1);

		entries = effectiveWith([
			{ key: 'alt+space', command: '-os.summon', scope: 'os' },
			{ key: 'ctrl+alt+space', command: 'os.summon', scope: 'os' },
		]);
		publish();
		expect(applied).toHaveLength(2);
		const summon = applied[1].filter((r) => r.command === 'os.summon');
		expect(summon).toEqual([{ command: 'os.summon', key: 'ctrl+alt+space' }]);
		stop();
	});

	it('a project OS rule never reaches the OS (E_OS_LAYER)', () => {
		const rules = osRulesFor(
			[...DEFAULT_KEYMAP, { command: 'os.summon', key: 'ctrl+alt+x', when: 'always', source: 'project', scope: 'os', label: 'x' }],
			'mac'
		);
		expect(rules.some((r) => r.key === 'ctrl+alt+x')).toBe(false);
	});
});
