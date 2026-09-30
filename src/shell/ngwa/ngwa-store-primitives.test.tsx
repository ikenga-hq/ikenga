// R57 · D-02 addendum — Store: catalog rows, Source chips, Q2 dedupe, the
// catalog sheet (closure → consent → trust → Install ▾ incl. Q5), Add from URL
// in every state, pin-mismatch handling and the Q4 Updates strip.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactElement } from 'react';
import { buildStoreCatalog, mergeCatalogIntoStore } from '@/lib/ngwa/enrichment';
import type { PrimitiveInstallOutcome } from '@/lib/ngwa/use-store-install';
import type { PrimitiveCatalogEntry } from '@/lib/registry/primitives';
import type { RegistryEntry } from '@/lib/registry/use-registry';
import type { ClaudeStoreEntry, ResolvedSource } from '@/lib/tauri-cmd';
import { NgwaStoreSurface, type NgwaStoreSurfaceProps } from './ngwa-store-surface';
import { isNpxSpec, nameFromSource, normalizeSource, urlKindBlock } from './ngwa-store-primitives';

afterEach(() => {
	cleanup();
	vi.useRealTimers();
});

const SHA = '9c41e07a1b2c3d4e5f60718293a4b5c6d7e8f901';
const NEW_SHA = '8b77f2d00112233445566778899aabbccddeeff0';
const HASH = `sha256-${'ab'.repeat(32)}`;

const cat = (name: string, over: Partial<PrimitiveCatalogEntry> = {}): PrimitiveCatalogEntry => ({
	kind: 'skill',
	name,
	version: '0.1.0',
	description: `${name} does things`,
	source: 'npx',
	url: `royalti-io/${name}`,
	publisher: 'royalti-io',
	...over,
});
const vaultEntry = (name: string, over: Partial<ClaudeStoreEntry> = {}): ClaudeStoreEntry => ({
	kind: 'skill',
	name,
	storePath: `/vault/skills/${name}`,
	description: null,
	modifiedMs: 0,
	enabledIn: ['workspace'],
	...over,
});

const CATALOG: PrimitiveCatalogEntry[] = [
	cat('groundwork'),
	cat('ikenga-artifact-builder', {
		requires: [
			{ kind: 'skill', name: 'design-language' },
			{ kind: 'skill', name: 'frontend-design', source: 'npx', ref: 'main' },
			{ kind: 'skill', name: 'impeccable' },
		],
	}),
	cat('design-language'),
	cat('impeccable'),
	cat('scrollytelling', { ref: SHA, hash: HASH }),
	cat('subagent-model-floor', {
		kind: 'hook',
		source: 'git',
		url: 'https://github.com/royalti-io/claude-hooks',
	}),
];
// impeccable sits in the vault (satisfied); scrollytelling is a catalog
// install behind its (moved) pin → the Updates strip.
const VAULT: ClaudeStoreEntry[] = [
	vaultEntry('impeccable', { enabledIn: ['project:other'] }),
	vaultEntry('scrollytelling', { version: '3e1a9c0ffff', fromCatalog: true }),
];
const REGISTRY = buildStoreCatalog(
	[],
	[
		{ name: '@ikenga/skill-groundwork', latest: '0.7.6', kind: 'skill' } as RegistryEntry,
		{ name: '@ikenga/pkg-tasks', latest: '0.8.3', kind: 'app' } as RegistryEntry,
	]
);

const OUTCOME: PrimitiveInstallOutcome = {
	scope: 'project:p1',
	placed: [
		{ kind: 'skill', name: 'ikenga-artifact-builder' },
		{ kind: 'skill', name: 'frontend-design' },
	],
	alsoEnabled: [{ kind: 'skill', name: 'impeccable' }],
	leftInPlace: [],
};

function renderStore(props: Partial<NgwaStoreSurfaceProps> = {}) {
	const { registry, primitives } = mergeCatalogIntoStore(REGISTRY, CATALOG, VAULT);
	const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	const all: NgwaStoreSurfaceProps = {
		catalog: registry,
		primitives,
		catalogEntries: CATALOG,
		vault: VAULT,
		catalogStatus: 'verified',
		activeProjectName: 'royalti-co',
		onInstallPrimitive: vi.fn().mockResolvedValue(OUTCOME),
		onUpdatePrimitive: vi.fn().mockResolvedValue(undefined),
		onResolveSource: vi.fn(),
		onInstallResolved: vi.fn(),
		onOpenInstalled: vi.fn(),
		...props,
	};
	const ui: ReactElement = (
		<QueryClientProvider client={client}>
			<NgwaStoreSurface {...all} />
		</QueryClientProvider>
	);
	render(ui);
	return all;
}

const sheet = () => screen.getByRole('region', { name: 'Install sheet' });
const row = (id: string) => document.querySelector(`.srow[data-id="${id}"]`) as HTMLElement;

describe('R57 · catalog rows in the Store', () => {
	it('lists catalog rows after the registry with the source and signed-catalog tags', () => {
		renderStore();
		const ids = [...document.querySelectorAll('.srow')].map((r) => r.getAttribute('data-id'));
		expect(ids).toEqual([
			'@ikenga/skill-groundwork',
			'@ikenga/pkg-tasks',
			'cat:skill:ikenga-artifact-builder',
			'cat:skill:design-language',
			'cat:skill:impeccable',
			'cat:skill:scrollytelling',
			'cat:hook:subagent-model-floor',
		]);
		const aib = row('cat:skill:ikenga-artifact-builder');
		expect(within(aib).getByText('npx · royalti-io/ikenga-artifact-builder')).toBeDefined();
		expect(within(aib).getByText('in the signed catalog')).toBeDefined();
		expect(within(aib).getByText('also installs 2 skills')).toBeDefined();
		expect(screen.getByText(/index \+ catalog signed/)).toBeDefined();
	});

	it('Q2: the registry twin wins and carries an `also: npx` note', () => {
		renderStore();
		expect(row('cat:skill:groundwork')).toBeNull();
		const gw = row('@ikenga/skill-groundwork');
		expect(within(gw).getByText('also: npx')).toBeDefined();
	});

	it('Source chips count and filter; Kind gains hook', () => {
		renderStore();
		const src = (id: string) => document.querySelector(`[data-source="${id}"]`) as HTMLElement;
		expect(src('registry').textContent).toContain('2');
		expect(src('git').textContent).toContain('1');
		expect(src('npx').textContent).toContain('4');
		const hook = document.querySelector('[data-kind="hook"]') as HTMLElement;
		expect(hook.textContent).toContain('1');
		expect(document.querySelector('[data-kind="mcp"]')).toBeNull();

		fireEvent.click(src('git'));
		expect([...document.querySelectorAll('.srow')].map((r) => r.getAttribute('data-id'))).toEqual([
			'cat:hook:subagent-model-floor',
		]);
		fireEvent.click(src('git'));
		expect(document.querySelectorAll('.srow')).toHaveLength(7);
	});

	it('a catalog that failed to verify is shown unavailable, never replaced by the seed', () => {
		renderStore({
			primitives: [],
			catalogStatus: 'error',
			catalogError: 'primitives.json signature did not verify',
		});
		expect(screen.getByText(/index signed · catalog unavailable/)).toBeDefined();
		const notice = document.querySelector('[data-catalog-unavailable]') as HTMLElement;
		expect(notice.textContent).toContain('signature did not verify');
		expect(notice.textContent).toContain('never replaced by the bundled copy');
	});
});

describe('R57 · the catalog sheet', () => {
	it('shows the closure before consent, gates Install on the non-catalog dep, and installs with Q5', async () => {
		const props = renderStore();
		fireEvent.click(row('cat:skill:ikenga-artifact-builder'));
		const s = sheet();
		const res = (name: string) =>
			(s.querySelector(`[data-dep="${name}"]`) as HTMLElement).getAttribute('data-res');
		expect(res('design-language')).toBe('catalog');
		expect(res('frontend-design')).toBe('pinned');
		expect(res('impeccable')).toBe('satisfied');
		expect(within(s).getByText('signed catalog')).toBeDefined();
		expect(within(s).getByText('not in the catalog · self-pinned')).toBeDefined();
		expect(within(s).getByText('already installed · not fetched')).toBeDefined();

		// Trust copy for an unpinned entry.
		expect(
			within(s).getByText('signed — vouches for the name and where it comes from')
		).toBeDefined();
		expect(within(s).getByText('unsigned — fetched from npx at install')).toBeDefined();
		expect(within(s).getByText('none — installs whatever the source serves today')).toBeDefined();
		expect(within(s).getByText('automatic — catalog installs follow the source')).toBeDefined();

		const install = within(s).getByRole('button', { name: 'Install to royalti-co' });
		expect((install as HTMLButtonElement).disabled).toBe(true);
		const box = s.querySelector('[data-consent="dep:frontend-design"]') as HTMLInputElement;
		expect(box.closest('.consent')?.textContent).toContain(
			'is not in the signed catalog. It is fetched from the source ikenga-artifact-builder names for it (npx @ main), which nothing here has reviewed.'
		);
		fireEvent.click(box);
		expect((install as HTMLButtonElement).disabled).toBe(false);

		await act(async () => {
			fireEvent.click(install);
		});
		expect(props.onInstallPrimitive).toHaveBeenCalledWith(
			expect.objectContaining({ name: 'ikenga-artifact-builder' }),
			'project',
			expect.any(Function)
		);
		await waitFor(() => expect(s.querySelector('[data-install-done]')).not.toBeNull());
		expect(
			within(s).getByText(/Installed ikenga-artifact-builder to royalti-co — plus 2 required items/)
		).toBeDefined();
		expect(s.querySelector('[data-also-enabled="impeccable"]')?.textContent).toContain(
			'also enabled in royalti-co'
		);
		fireEvent.click(within(s).getByRole('button', { name: 'Open in Installed' }));
		expect(props.onOpenInstalled).toHaveBeenCalledWith('ikenga-artifact-builder');
	});

	it('a pinned entry states its pin and the pin-moves-only update policy', () => {
		renderStore();
		fireEvent.click(row('cat:skill:scrollytelling'));
		const s = sheet();
		expect(s.querySelector('[data-pinned-version]')?.textContent).toContain('9c41e07');
		expect(within(s).getByText('moves only when the signed catalog moves the pin')).toBeDefined();
	});

	it('a hook row is installable from git behind one Share-kola consent', () => {
		renderStore();
		fireEvent.click(row('cat:hook:subagent-model-floor'));
		const s = sheet();
		const install = within(s).getByRole('button', {
			name: 'Install to royalti-co',
		}) as HTMLButtonElement;
		expect(install.disabled).toBe(true);
		// The foot says why, with the live count (not only a tooltip).
		expect(s.querySelector('[data-install-blocked]')?.textContent).toBe(
			'Tick every consent above first (0 of 1 ticked)'
		);
		fireEvent.click(s.querySelector('[data-consent="runs"]') as HTMLInputElement);
		expect(install.disabled).toBe(false);
		expect(s.querySelector('[data-install-blocked]')).toBeNull();
	});

	it('a pin mismatch at install is its own state: nothing written, re-check the catalog', async () => {
		const recheck = vi.fn();
		renderStore({
			onInstallPrimitive: vi
				.fn()
				.mockRejectedValue(new Error('pin mismatch: expected 9c41e07, got 8b77f2d')),
			onRecheckCatalog: recheck,
		});
		fireEvent.click(row('cat:skill:design-language'));
		await act(async () => {
			fireEvent.click(within(sheet()).getByRole('button', { name: 'Install to royalti-co' }));
		});
		const err = await waitFor(() => {
			const e = sheet().querySelector('[data-install-error]');
			if (!e) throw new Error('no error state yet');
			return e as HTMLElement;
		});
		expect(err.textContent).toContain('The source no longer serves the pinned version');
		expect(err.textContent).toContain('pin mismatch: expected 9c41e07, got 8b77f2d');
		expect(err.textContent).toContain('Nothing was written');
		fireEvent.click(within(sheet()).getByRole('button', { name: 'Re-check the catalog' }));
		expect(recheck).toHaveBeenCalled();
	});
});

describe('R57 · Q4 Updates strip', () => {
	it('a catalog install whose pin moved joins the strip and updates to the pin', async () => {
		const props = renderStore();
		const strip = document.querySelector('[data-updates]') as HTMLElement;
		expect(strip.textContent).toContain('1 update available');
		expect(strip.textContent).toContain('scrollytelling 3e1a9c0 → 9c41e07');
		fireEvent.click(screen.getByRole('button', { name: 'Update all (1)' }));
		const dialog = screen.getByRole('dialog');
		expect(within(dialog).getByText('moved by the signed catalog')).toBeDefined();
		await act(async () => {
			fireEvent.click(within(dialog).getByRole('button', { name: 'Update all (1)' }));
		});
		expect(props.onUpdatePrimitive).toHaveBeenCalledWith(
			expect.objectContaining({ name: 'scrollytelling' })
		);
	});
});

// ── Add from URL ────────────────────────────────────────────────────────────

const RESOLVED: ResolvedSource = {
	kind: 'skill',
	name: 'liner-notes',
	inferredFrom: 'SKILL.md at the repo root',
	source: 'git',
	url: 'https://github.com/kolanut-labs/liner-notes',
	ref: null,
	sha: SHA,
	hash: HASH,
	files: ['SKILL.md', 'templates/', 'examples/', 'README.md'],
	requires: [
		{ kind: 'skill', name: 'ikenga-artifact-builder' },
		{ kind: 'skill', name: 'credits-parse', source: 'git', ref: 'v2' },
		{ kind: 'skill', name: 'impeccable' },
	],
	description: null,
	trust: 'unsigned',
};

function openAddUrl() {
	fireEvent.click(screen.getByRole('button', { name: /Add from URL/ }));
	return sheet();
}
const urlState = () => sheet().querySelector('[data-addurl-sheet]')?.getAttribute('data-state');
const sourceInput = () => screen.getByLabelText('git URL or npx package') as HTMLInputElement;
const kindSeg = (k: string) =>
	document.querySelector(`[role="radiogroup"] [data-k="${k}"]`) as HTMLButtonElement;

describe('R57 · Add from URL', () => {
	it('helpers: npx vs git, name derivation, kind rules', () => {
		expect(isNpxSpec('kolanut-labs/liner-notes')).toBe(true);
		expect(isNpxSpec('npx skills add kolanut-labs/liner-notes')).toBe(true);
		expect(normalizeSource('npx skills add a/b')).toBe('a/b');
		expect(isNpxSpec('https://github.com/a/b')).toBe(false);
		expect(isNpxSpec('git@github.com:a/b.git')).toBe(false);
		expect(nameFromSource('https://github.com/a/liner-notes.git')).toBe('liner-notes');
		expect(nameFromSource('git@github.com:a/b.git')).toBe('b');
		expect(urlKindBlock('hook', false)).toBeNull();
		expect(urlKindBlock('mcp', false)).toBeNull();
		expect(urlKindBlock('agent', true)).toMatch(/npx installs skills only/);
		expect(urlKindBlock('skill', true)).toBeNull();
	});

	it('empty: sits beside search, Resolve waits for a source, the suffix and name follow the URL', () => {
		renderStore();
		const btn = screen.getByRole('button', { name: /Add from URL/ });
		expect(btn.previousElementSibling?.classList.contains('search')).toBe(true);
		const s = openAddUrl();
		expect(btn.getAttribute('aria-expanded')).toBe('true');
		expect(within(s).getByRole('heading', { name: 'Add from URL' })).toBeDefined();
		expect(s.textContent).toContain(
			'Nothing is placed, and nothing in your scopes changes, until you Install.'
		);
		const resolve = within(s).getByRole('button', { name: 'Resolve' }) as HTMLButtonElement;
		expect(resolve.disabled).toBe(true);

		fireEvent.change(sourceInput(), {
			target: { value: 'https://github.com/kolanut-labs/liner-notes' },
		});
		expect(resolve.disabled).toBe(false);
		expect(s.querySelector('[data-route]')?.textContent).toBe('git');
		expect((screen.getByLabelText('Name') as HTMLInputElement).value).toBe('liner-notes');
		expect(screen.getByLabelText('Git ref')).toBeDefined();
		// git: hook and mcp are enabled (N-A); every kind is.
		for (const k of ['skill', 'agent', 'command', 'hook', 'mcp'])
			expect(kindSeg(k).disabled).toBe(false);
	});

	it('Kind rules for an npx spec: skills only, no Ref field', () => {
		renderStore();
		openAddUrl();
		fireEvent.click(kindSeg('agent'));
		fireEvent.change(sourceInput(), { target: { value: 'kolanut-labs/liner-notes' } });
		expect(document.querySelector('[data-route]')?.textContent).toBe('npx');
		// The chosen kind fell back to Infer when npx ruled it out.
		expect(kindSeg('infer').getAttribute('aria-checked')).toBe('true');
		for (const k of ['agent', 'command', 'hook', 'mcp']) {
			expect(kindSeg(k).disabled).toBe(true);
			expect(kindSeg(k).title).toMatch(/npx installs skills only/);
		}
		expect(kindSeg('skill').disabled).toBe(false);
		expect(screen.queryByLabelText('Git ref')).toBeNull();
	});

	it('an empty search offers Add from URL and carries the query into Source', () => {
		renderStore();
		fireEvent.change(screen.getByLabelText('Search the registry'), {
			target: { value: 'kolanut-labs/liner-notes' },
		});
		const empty = document.querySelector('[data-store-empty]') as HTMLElement;
		fireEvent.click(within(empty).getByRole('button', { name: /Add from URL/ }));
		expect(sourceInput().value).toBe('kolanut-labs/liner-notes');
		expect((screen.getByLabelText('Name') as HTMLInputElement).value).toBe('liner-notes');
	});

	it('resolving → resolved → installing → done, pinned to what was resolved', async () => {
		let finishResolve: (r: ResolvedSource) => void = () => {};
		let finishInstall: (o: PrimitiveInstallOutcome) => void = () => {};
		let stage: (s: 'fetch' | 'place') => void = () => {};
		const props = renderStore({
			onResolveSource: vi.fn(
				() =>
					new Promise<ResolvedSource>((res) => {
						finishResolve = res;
					})
			),
			onInstallResolved: vi.fn(
				(_r, _s, onStage) =>
					new Promise<PrimitiveInstallOutcome>((res) => {
						stage = onStage;
						finishInstall = res;
					})
			),
		});
		const s = openAddUrl();
		fireEvent.change(sourceInput(), {
			target: { value: 'https://github.com/kolanut-labs/liner-notes' },
		});
		fireEvent.change(screen.getByLabelText('Git ref'), { target: { value: 'main' } });
		fireEvent.click(within(s).getByRole('button', { name: 'Resolve' }));

		// resolving
		expect(urlState()).toBe('resolving');
		expect(s.parentElement?.querySelector('[data-stage]')?.textContent).toContain('Cloning');
		expect(sourceInput().readOnly).toBe(true);
		expect(props.onResolveSource).toHaveBeenCalledWith(
			'https://github.com/kolanut-labs/liner-notes',
			{
				kind: null,
				name: 'liner-notes',
				gitRef: 'main',
			}
		);

		// resolved
		await act(async () => finishResolve(RESOLVED));
		expect(urlState()).toBe('resolved');
		expect(within(sheet()).getByRole('heading', { name: 'liner-notes' })).toBeDefined();
		expect(s.textContent).toContain('inferred · SKILL.md at the repo root');
		expect(s.textContent).toContain('9c41e07');
		expect(s.textContent).toContain('4 files · SKILL.md, templates/, examples/, README.md');
		expect(s.querySelector('[data-dep="ikenga-artifact-builder"]')?.getAttribute('data-res')).toBe(
			'catalog'
		);
		expect(s.querySelector('[data-dep="credits-parse"]')?.getAttribute('data-res')).toBe('pinned');
		expect(s.querySelector('[data-dep="impeccable"]')?.getAttribute('data-res')).toBe('satisfied');
		const warn = s.querySelector('[data-unsigned-warning]') as HTMLElement;
		expect(warn.textContent).toContain('Unsigned — review it before you install');
		expect(warn.textContent).toContain(
			'It is not in the signed catalog, and its files carry no signature.'
		);
		expect(s.textContent).toContain(
			'I have read it, or I trust whoever published it. Pinned to 9c41e07 — what I install is what was resolved.'
		);
		expect(s.textContent).toContain(
			'A direct install never updates itself. Update re-fetches only when you ask, and shows the new version first.'
		);
		const install = within(sheet()).getByRole('button', {
			name: 'Install to royalti-co',
		}) as HTMLButtonElement;
		expect(install.disabled).toBe(true);
		fireEvent.click(s.querySelector('[data-consent="src"]') as HTMLInputElement);
		expect(install.disabled).toBe(true);
		expect(sheet().querySelector('[data-install-blocked]')?.textContent).toMatch(
			/^Tick every box under Share kola first \(1 of \d+ ticked\)$/
		);
		fireEvent.click(s.querySelector('[data-consent="dep:credits-parse"]') as HTMLInputElement);
		// The catalogued ikenga-artifact-builder reveals its own non-catalog dep
		// (transitive closure), which needs its own box too.
		expect(s.querySelector('[data-dep="frontend-design"]')?.getAttribute('data-res')).toBe(
			'pinned'
		);
		expect(install.disabled).toBe(true);
		fireEvent.click(s.querySelector('[data-consent="dep:frontend-design"]') as HTMLInputElement);
		expect(install.disabled).toBe(false);

		// installing
		await act(async () => {
			fireEvent.click(install);
		});
		expect(urlState()).toBe('installing');
		expect(props.onInstallResolved).toHaveBeenCalledWith(RESOLVED, 'project', expect.any(Function));
		expect(sheet().querySelector('[data-stage]')?.textContent).toContain(
			'Fetching 9c41e07 → Materializing deps'
		);
		await act(async () => stage('place'));
		expect(sheet().querySelector('[data-stage]')?.textContent).toContain('Placing in royalti-co');

		// done
		await act(async () =>
			finishInstall({
				scope: 'project:p1',
				placed: [
					{ kind: 'skill', name: 'liner-notes' },
					{ kind: 'skill', name: 'credits-parse' },
				],
				alsoEnabled: [],
				leftInPlace: [{ kind: 'skill', name: 'impeccable' }],
			})
		);
		expect(urlState()).toBe('done');
		expect(s.textContent).toContain('Installed liner-notes to royalti-co — plus 1 required item');
		expect(s.textContent).toContain('Auto-update is off because this came from a URL.');
		expect(s.querySelector('[data-placed-item="liner-notes"]')?.textContent).toContain(
			'royalti-co/.claude/skills/liner-notes'
		);
		expect(s.querySelector('[data-left="impeccable"]')?.textContent).toContain('left where it is');
		fireEvent.click(within(sheet()).getByRole('button', { name: 'Add another' }));
		expect(urlState()).toBe('empty');
		expect(sourceInput().value).toBe('');
	});

	it('Edit returns to the fields with everything kept', async () => {
		renderStore({ onResolveSource: vi.fn().mockResolvedValue(RESOLVED) });
		const s = openAddUrl();
		fireEvent.change(sourceInput(), {
			target: { value: 'https://github.com/kolanut-labs/liner-notes' },
		});
		await act(async () => {
			fireEvent.click(within(s).getByRole('button', { name: 'Resolve' }));
		});
		fireEvent.click(within(sheet()).getByRole('button', { name: 'Edit' }));
		expect(sourceInput().value).toBe('https://github.com/kolanut-labs/liner-notes');
		expect((screen.getByLabelText('Name') as HTMLInputElement).value).toBe('liner-notes');
	});

	it('error: the backend string verbatim, what to try, nothing written, Resolve again', async () => {
		const msg =
			'no SKILL.md found in clone for skill "liner-notes" (looked at root, skills/liner-notes, liner-notes)';
		const resolve = vi.fn().mockRejectedValueOnce(new Error(msg)).mockResolvedValueOnce(RESOLVED);
		renderStore({ onResolveSource: resolve });
		const s = openAddUrl();
		fireEvent.change(sourceInput(), {
			target: { value: 'https://github.com/kolanut-labs/liner-notes' },
		});
		await act(async () => {
			fireEvent.click(within(s).getByRole('button', { name: 'Resolve' }));
		});
		expect(urlState()).toBe('error');
		expect(s.querySelector('[data-backend-error]')?.textContent).toBe(msg);
		expect(s.textContent).toContain('Nothing was written — the fetch stayed in a staging folder');
		expect(s.textContent).toContain('Pick the kind');
		await act(async () => {
			fireEvent.click(within(sheet()).getByRole('button', { name: 'Resolve again' }));
		});
		expect(resolve).toHaveBeenCalledTimes(2);
		expect(urlState()).toBe('resolved');
	});

	it('a pin mismatch at install is an error state that explains the move', async () => {
		renderStore({
			onResolveSource: vi.fn().mockResolvedValue({ ...RESOLVED, requires: [] }),
			onInstallResolved: vi
				.fn()
				.mockRejectedValue(
					new Error(`pin mismatch: expected ${SHA}, the source serves ${NEW_SHA}`)
				),
		});
		const s = openAddUrl();
		fireEvent.change(sourceInput(), {
			target: { value: 'https://github.com/kolanut-labs/liner-notes' },
		});
		await act(async () => {
			fireEvent.click(within(s).getByRole('button', { name: 'Resolve' }));
		});
		fireEvent.click(s.querySelector('[data-consent="src"]') as HTMLInputElement);
		await act(async () => {
			fireEvent.click(within(sheet()).getByRole('button', { name: 'Install to royalti-co' }));
		});
		expect(urlState()).toBe('error');
		const err = s.querySelector('[data-url-error]') as HTMLElement;
		expect(err.textContent).toContain('The source moved since Resolve');
		expect(err.textContent).toContain('The source no longer serves 9c41e07');
		expect(err.textContent).toContain('Nothing was written');
		expect(within(sheet()).getByRole('button', { name: 'Resolve again' })).toBeDefined();
	});

	it('another install error keeps the resolved review and says why', async () => {
		renderStore({
			onResolveSource: vi.fn().mockResolvedValue({ ...RESOLVED, requires: [] }),
			onInstallResolved: vi.fn().mockRejectedValue(new Error('network unreachable')),
		});
		const s = openAddUrl();
		fireEvent.change(sourceInput(), {
			target: { value: 'https://github.com/kolanut-labs/liner-notes' },
		});
		await act(async () => {
			fireEvent.click(within(s).getByRole('button', { name: 'Resolve' }));
		});
		fireEvent.click(s.querySelector('[data-consent="src"]') as HTMLInputElement);
		await act(async () => {
			fireEvent.click(within(sheet()).getByRole('button', { name: 'Install to royalti-co' }));
		});
		expect(urlState()).toBe('resolved');
		expect(sheet().querySelector('[data-action-error]')?.textContent).toContain(
			'network unreachable'
		);
	});

	it('the resolving stages advance while the one call runs', () => {
		vi.useFakeTimers();
		renderStore({ onResolveSource: vi.fn(() => new Promise<ResolvedSource>(() => {})) });
		const s = openAddUrl();
		fireEvent.change(sourceInput(), { target: { value: 'kolanut-labs/liner-notes' } });
		fireEvent.click(within(s).getByRole('button', { name: 'Resolve' }));
		const stageText = () => sheet().querySelector('[data-stage]')?.textContent ?? '';
		expect(stageText()).toContain('npx skills add');
		act(() => {
			vi.advanceTimersByTime(1300);
		});
		expect(stageText()).toContain('Locating SKILL.md');
		act(() => {
			vi.advanceTimersByTime(5000);
		});
		expect(stageText()).toContain('Reading requires');
	});
});
