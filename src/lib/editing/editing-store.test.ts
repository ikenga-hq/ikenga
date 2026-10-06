// Stash ownership in the editing store (plans/file-editing): a remounting
// editor claims a stashed draft when its load starts — so an unmount before
// the load finishes can put it back — and a save that lands after its editor
// unmounted moves the stash onto what it wrote.

import { beforeEach, describe, expect, it } from 'vitest';
import {
	anyDirtySession,
	dirtySessionKeys,
	type StashedDraft,
	sessionKey,
	useEditingStore,
} from './editing-store';

const meta = { eol: '\n' as const, bom: false };
const stash = (draft: string, base: string): StashedDraft => ({ draft, base, meta });
const store = () => useEditingStore.getState();

beforeEach(() => {
	useEditingStore.setState({ sessions: {} });
});

describe('claimStash', () => {
	it("takes this pane's stash and keeps it on the session, now mounted", () => {
		const key = sessionKey('/w/a.md', 'p1');
		store().upsert(key, {
			path: '/w/a.md',
			paneId: 'p1',
			mounted: false,
			dirty: true,
			stash: stash('mine', 'base'),
		});
		expect(store().claimStash(key, '/w/a.md', 'p1')).toEqual(stash('mine', 'base'));
		expect(store().sessions[key]).toMatchObject({ mounted: true, stash: stash('mine', 'base') });
		// Still unsaved while the editor loads.
		expect(dirtySessionKeys([{ path: '/w/a.md', paneId: 'p1' }])).toEqual([key]);
		expect(anyDirtySession()).toBe(true);
	});

	it('moves an unmounted stash from another pane (the tab moved panes)', () => {
		const from = sessionKey('/w/a.md', 'p1');
		const to = sessionKey('/w/a.md', 'p2');
		store().upsert(from, {
			path: '/w/a.md',
			paneId: 'p1',
			mounted: false,
			dirty: true,
			stash: stash('mine', 'base'),
		});
		expect(store().claimStash(to, '/w/a.md', 'p2')?.draft).toBe('mine');
		expect(store().sessions[from]).toBeUndefined();
		expect(store().sessions[to]).toMatchObject({ paneId: 'p2', mounted: true });
	});

	it('never takes a stash held by a mounted editor in another pane', () => {
		store().upsert(sessionKey('/w/a.md', 'p1'), {
			path: '/w/a.md',
			paneId: 'p1',
			mounted: true,
			stash: stash('held', 'base'),
		});
		expect(store().claimStash(sessionKey('/w/a.md', 'p2'), '/w/a.md', 'p2')).toBeNull();
		expect(store().sessions[sessionKey('/w/a.md', 'p1')]?.stash?.draft).toBe('held');
	});

	it('returns null when there is nothing stashed', () => {
		expect(store().claimStash(sessionKey('/w/a.md', 'p1'), '/w/a.md', 'p1')).toBeNull();
		expect(store().sessions).toEqual({});
	});
});

describe('rebaseStash', () => {
	const key = sessionKey('/w/a.md', 'p1');
	const unmounted = (s: StashedDraft) =>
		store().upsert(key, { path: '/w/a.md', paneId: 'p1', mounted: false, dirty: true, stash: s });

	it('drops the stash when the draft is exactly what was written', () => {
		unmounted(stash('mine', 'base'));
		store().rebaseStash(key, 'base', 'mine', meta);
		expect(store().sessions[key]).toBeUndefined();
		expect(anyDirtySession()).toBe(false);
	});

	it('moves a newer draft onto what was written', () => {
		unmounted(stash('mine and more', 'base'));
		store().rebaseStash(key, 'base', 'mine', { eol: '\r\n', bom: false });
		expect(store().sessions[key]?.stash).toEqual({
			draft: 'mine and more',
			base: 'mine',
			meta: { eol: '\r\n', bom: false },
		});
	});

	it('leaves a stash on another base, or one a remounted editor owns, alone', () => {
		unmounted(stash('mine', 'other'));
		store().rebaseStash(key, 'base', 'mine', meta);
		expect(store().sessions[key]?.stash?.base).toBe('other');

		store().upsert(key, {
			path: '/w/a.md',
			paneId: 'p1',
			mounted: true,
			stash: stash('m', 'base'),
		});
		store().rebaseStash(key, 'base', 'm', meta);
		expect(store().sessions[key]?.stash).toEqual(stash('m', 'base'));
	});
});
