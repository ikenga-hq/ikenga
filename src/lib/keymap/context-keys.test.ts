import { afterEach, describe, expect, it } from 'vitest';
import {
	buildMenuContext,
	CONTEXT_KEYS,
	type ContextInputs,
	computeContextKeys,
	extnameOf,
	FOCUS_CONTEXT_KEYS,
	focusMarkerProps,
	getContextKeys,
	isFocusWithin,
	isKnownContextKey,
	isPaletteOpen,
	setPaletteOpen,
} from './context-keys';
import { evaluateWhen, isTypingTarget } from './when';

function inputs(over: Partial<ContextInputs> = {}): ContextInputs {
	return {
		activeElement: null,
		pane: null,
		filesSelectedPath: null,
		project: null,
		paletteOpen: false,
		...over,
	};
}

/** Mount `html` and return the element matching `pick`. */
function mount(html: string, pick: string): HTMLElement {
	const host = document.createElement('div');
	host.innerHTML = html;
	document.body.appendChild(host);
	const el = host.querySelector<HTMLElement>(pick);
	if (!el) throw new Error(`no ${pick}`);
	return el;
}

afterEach(() => {
	document.body.innerHTML = '';
	setPaletteOpen(false);
});

describe('vocabulary (§4.3)', () => {
	it('is exactly the frozen DEC-62 keys plus the freeze additions and WP-56\'s B-21 additions', () => {
		expect(Object.keys(CONTEXT_KEYS).sort()).toEqual(
			[
				'inputFocus',
				'terminalFocus',
				'explorerFocus',
				'filesFocus',
				'paneFocus',
				'paneKind',
				'resource',
				'resourceExtname',
				'project',
				'sessionFocus',
				'ngwaItemFocus',
				'ngwaItemKind',
				'dispatchFocus',
				'paletteOpen',
				// WP-56 (B-21 additive rule, G-ACTIONS §10.2 "Reserved for WP-56").
				'permissionCardFocus',
				'approveGateFocus',
				'loupeFocus',
				'pinComposerFocus',
				'markdownEditorFocus',
				// Fix round 1 (B-21): narrower than approveGateFocus (detail pane only).
				'approveGateDetailFocus',
			].sort()
		);
		expect(isKnownContextKey('ngwaItemKind')).toBe(true);
		expect(isKnownContextKey('toString')).toBe(false);
		expect(isKnownContextKey('editorFocus')).toBe(false);
	});

	it('marks the focus keys (undefined in a menu context)', () => {
		expect([...FOCUS_CONTEXT_KEYS].sort()).toEqual(
			[
				'inputFocus',
				'terminalFocus',
				'explorerFocus',
				'filesFocus',
				'paneFocus',
				'sessionFocus',
				'ngwaItemFocus',
				'dispatchFocus',
				'paletteOpen',
				'permissionCardFocus',
				'approveGateFocus',
				'loupeFocus',
				'pinComposerFocus',
				'markdownEditorFocus',
				'approveGateDetailFocus',
			].sort()
		);
	});

	it('always produces every key', () => {
		expect(Object.keys(computeContextKeys(inputs())).sort()).toEqual(Object.keys(CONTEXT_KEYS).sort());
	});
});

describe('inputFocus — exactly isTypingTarget()', () => {
	it.each([
		['<input id="t">', true],
		['<textarea id="t"></textarea>', true],
		['<div id="t" contenteditable="true"></div>', true],
		['<button id="t">x</button>', false],
		['<div id="t" contenteditable="false"></div>', false],
	])('%s → %s', (html, expected) => {
		const el = mount(html, '#t');
		expect(computeContextKeys(inputs({ activeElement: el })).inputFocus).toBe(expected);
		expect(isTypingTarget(el)).toBe(expected);
	});

	it('is false with nothing focused', () => {
		expect(computeContextKeys(inputs()).inputFocus).toBe(false);
	});
});

describe('focus-within markers and shipped-DOM fallbacks', () => {
	it('terminalFocus: inside an xterm host (its helper textarea is also inputFocus)', () => {
		const el = mount('<div data-terminal-session="s1"><div class="xterm"><textarea id="t"></textarea></div></div>', '#t');
		const ctx = computeContextKeys(inputs({ activeElement: el }));
		expect(ctx.terminalFocus).toBe(true);
		expect(ctx.inputFocus).toBe(true);
	});

	it('dispatchFocus: the Companion dispatch input', () => {
		const el = mount('<div data-companion-dispatch=""><textarea id="t"></textarea></div>', '#t');
		expect(computeContextKeys(inputs({ activeElement: el })).dispatchFocus).toBe(true);
	});

	it('honours data-ctx-focus markers (space-separated)', () => {
		const el = mount(`<div data-ctx-focus="${focusMarkerProps('dispatch', 'pane')['data-ctx-focus']}"><input id="t"></div>`, '#t');
		expect(isFocusWithin(el, 'dispatch')).toBe(true);
		expect(isFocusWithin(el, 'pane')).toBe(true);
		expect(isFocusWithin(el, 'terminal')).toBe(false);
	});

	it('filesFocus: a Files row (not its header), implying explorerFocus, with the row resource', () => {
		const html = `
			<div data-explorer-section="files">
				<div data-explorer-row="header"><button id="h">Files</button></div>
				<div tabindex="0" id="r" data-ctx-resource="/p/src/Main.TS">Main.TS</div>
			</div>`;
		const row = mount(html, '#r');
		const ctx = computeContextKeys(inputs({ activeElement: row, filesSelectedPath: '/p/other.rs' }));
		expect(ctx.filesFocus).toBe(true);
		expect(ctx.explorerFocus).toBe(true);
		expect(ctx.paneFocus).toBe(false);
		expect(ctx.resource).toBe('/p/src/Main.TS');
		expect(ctx.resourceExtname).toBe('.ts');

		const header = document.querySelector<HTMLElement>('#h');
		const hctx = computeContextKeys(inputs({ activeElement: header }));
		expect(hctx.filesFocus).toBe(false);
		expect(hctx.explorerFocus).toBe(true);
		expect(hctx.resource).toBeUndefined();
	});

	it('filesFocus falls back to the files-store selection for a row without data-ctx-resource', () => {
		const row = mount('<div data-explorer-section="files"><div tabindex="0" id="r">x</div></div>', '#r');
		const ctx = computeContextKeys(inputs({ activeElement: row, filesSelectedPath: '/p/README' }));
		expect(ctx.resource).toBe('/p/README');
		expect(ctx.resourceExtname).toBe('');
	});

	it('sessionFocus: a Sessions row', () => {
		const row = mount('<div data-explorer-section="sessions"><div tabindex="0" id="r">s</div></div>', '#r');
		const ctx = computeContextKeys(inputs({ activeElement: row }));
		expect(ctx.sessionFocus).toBe(true);
		expect(ctx.explorerFocus).toBe(true);
		expect(ctx.filesFocus).toBe(false);
	});

	it('ngwaItemFocus + ngwaItemKind: an Ngwa item row', () => {
		const row = mount(
			'<div data-explorer-section="ngwa-project"><div tabindex="0" id="r" data-ctx-ngwa-kind="skill">s</div></div>',
			'#r'
		);
		const ctx = computeContextKeys(inputs({ activeElement: row }));
		expect(ctx.ngwaItemFocus).toBe(true);
		expect(ctx.ngwaItemKind).toBe('skill');
		expect(
			evaluateWhen("ngwaItemFocus && (ngwaItemKind == 'skill' || ngwaItemKind == 'mcp')", ctx)
		).toBe(true);
	});

	it('ngwaItemFocus via a marker outside the Explorer (the Ngwa lists)', () => {
		const row = mount('<ul><li tabindex="0" id="r" data-ctx-focus="ngwa-item" data-ctx-ngwa-kind="mcp">m</li></ul>', '#r');
		const ctx = computeContextKeys(inputs({ activeElement: row }));
		expect(ctx.ngwaItemFocus).toBe(true);
		expect(ctx.ngwaItemKind).toBe('mcp');
		expect(ctx.explorerFocus).toBe(false);
	});

	it('ngwaItemKind is undefined without ngwaItemFocus', () => {
		const el = mount('<div data-ctx-ngwa-kind="skill"><button id="b">x</button></div>', '#b');
		expect(computeContextKeys(inputs({ activeElement: el })).ngwaItemKind).toBeUndefined();
	});
});

describe('pane derivation', () => {
	it('paneFocus + resource from the focused leaf\'s active tab', () => {
		const el = mount('<div data-pane-id="p1"><button id="b">x</button></div>', '#b');
		const ctx = computeContextKeys(inputs({ activeElement: el, pane: { kind: 'artifact', path: '/p/a.html' } }));
		expect(ctx.paneFocus).toBe(true);
		expect(ctx.paneKind).toBe('artifact');
		expect(ctx.resource).toBe('/p/a.html');
		expect(ctx.resourceExtname).toBe('.html');
	});

	it('a terminal tab has no resource', () => {
		const el = mount('<div data-pane-id="p1"><div data-terminal-session="s"><textarea id="t"></textarea></div></div>', '#t');
		const ctx = computeContextKeys(inputs({ activeElement: el, pane: { kind: 'terminal', sessionId: 's' } }));
		expect(ctx.paneFocus).toBe(true);
		expect(ctx.terminalFocus).toBe(true);
		expect(ctx.paneKind).toBe('terminal');
		expect(ctx.resource).toBeUndefined();
		expect(ctx.resourceExtname).toBeUndefined();
	});

	it('paneKind follows the focused leaf even when DOM focus is elsewhere; resource does not', () => {
		const el = mount('<div data-explorer-section="views"><button id="b">x</button></div>', '#b');
		const ctx = computeContextKeys(inputs({ activeElement: el, pane: { kind: 'route', path: '/settings' } }));
		expect(ctx.paneFocus).toBe(false);
		expect(ctx.paneKind).toBe('route');
		expect(ctx.resource).toBeUndefined();
	});

	it('paneKind is undefined with no pane', () => {
		expect(computeContextKeys(inputs()).paneKind).toBeUndefined();
	});
});

describe('project and palette', () => {
	it('project is the active id, undefined for a path-less project', () => {
		expect(computeContextKeys(inputs({ project: { id: 'p1', rootPath: '/p' } })).project).toBe('p1');
		expect(computeContextKeys(inputs({ project: { id: 'default', rootPath: null } })).project).toBeUndefined();
	});

	it('paletteOpen comes from the palette state', () => {
		expect(isPaletteOpen()).toBe(false);
		setPaletteOpen(true);
		expect(isPaletteOpen()).toBe(true);
		expect(getContextKeys().paletteOpen).toBe(true);
		expect(computeContextKeys(inputs({ paletteOpen: false })).paletteOpen).toBe(false);
	});
});

describe('extnameOf', () => {
	it.each([
		['/a/b/c.TS', '.ts'],
		['/a/b/c.tar.gz', '.gz'],
		['/a/b/.gitignore', ''],
		['/a/b/Makefile', ''],
		['C:\\a\\b.Rs', '.rs'],
		['/a.b/c', ''],
	])('%j → %j', (p, e) => {
		expect(extnameOf(p)).toBe(e);
	});

	it('is undefined for an undefined resource', () => {
		expect(extnameOf(undefined)).toBeUndefined();
	});
});

describe('menu context (§1.3)', () => {
	it('binds the row, leaves every focus key undefined', () => {
		const ctx = buildMenuContext({ resource: '/p/src/a.ts', paneKind: 'artifact' });
		expect(ctx.resourceExtname).toBe('.ts');
		for (const k of FOCUS_CONTEXT_KEYS) expect(ctx[k]).toBeUndefined();
		expect(evaluateWhen("resource =~ '*.{ts,rs}'", ctx)).toBe(true);
		// A placement naming a focus key never shows (W_FOCUS_IN_PLACEMENT).
		expect(evaluateWhen("filesFocus && resource =~ '*.ts'", ctx)).toBe(false);
	});
});

describe('getContextKeys (live)', () => {
	it('reads the event target over document.activeElement', () => {
		const el = mount('<input id="t">', '#t');
		expect(getContextKeys(el).inputFocus).toBe(true);
		expect(getContextKeys(document.body).inputFocus).toBe(false);
	});

	it('produces every key from the live stores', () => {
		expect(Object.keys(getContextKeys()).sort()).toEqual(Object.keys(CONTEXT_KEYS).sort());
	});
});
