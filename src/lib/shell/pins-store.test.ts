import { beforeEach, describe, expect, it, vi } from 'vitest';
import { renderHook } from '@testing-library/react';

// Hoisted mocks for the tauri-cmd module — vitest's vi.mock is hoisted, so
// we expose our state via the factory return value and reach into it via
// the `mocked` import below.
vi.mock('@/lib/tauri-cmd', () => {
	const state = {
		pins: [] as Array<Record<string, unknown>>,
		sections: [] as Array<Record<string, unknown>>,
		nextId: 1,
	};
	return {
		activityPinsList: vi.fn(async () => state.pins.slice()),
		activitySectionsList: vi.fn(async () => state.sections.slice()),
		activityPinsAdd: vi.fn(async (args: Record<string, unknown>) => {
			// Validate section exists when provided (mirrors the Rust check so
			// store tests can exercise the missing-section path).
			if (args.sectionId && !state.sections.some((s) => s.id === args.sectionId)) {
				throw new Error(`section '${args.sectionId}' does not exist`);
			}
			// Mirror the host's manifest_id uniqueness check so store tests
			// can exercise the duplicate path (otherwise the FE would accept
			// the add and the host would reject it on the real call).
			if (args.manifestId && state.pins.some((p) => p.manifestId === args.manifestId)) {
				throw new Error(`manifest_id '${args.manifestId}' is already pinned`);
			}
			const sortOrder = state.pins.filter((p) =>
				(args.sectionId ?? null) === null ? p.sectionId === null : p.sectionId === args.sectionId
			).length;
			const pin = {
				id: `pin-${state.nextId++}`,
				kind: args.kind,
				target: args.target,
				label: args.label,
				iconLucide: args.iconLucide ?? null,
				iconEmoji: args.iconEmoji ?? null,
				sectionId: args.sectionId ?? null,
				sortOrder,
				createdAt: '2026-05-10T00:00:00Z',
				manifestId: args.manifestId ?? null,
				lastOpenedAt: null,
			};
			state.pins.push(pin);
			return pin;
		}),
		activityPinsRemove: vi.fn(async (id: string) => {
			state.pins = state.pins.filter((p) => p.id !== id);
		}),
		activityPinsReorder: vi.fn(async (orderedIds: string[], sectionId: string) => {
			const targetSection = sectionId === '' ? null : sectionId;
			orderedIds.forEach((id, idx) => {
				const p = state.pins.find((p) => p.id === id);
				if (p) {
					p.sortOrder = idx;
					p.sectionId = targetSection;
				}
			});
		}),
		activitySectionsCreate: vi.fn(async (args: Record<string, unknown>) => {
			if (args.id === 'system' || args.id === 'settings') {
				throw new Error(`'${args.id}' is a reserved section id`);
			}
			const section = {
				id: args.id,
				label: args.label,
				iconLucide: args.iconLucide ?? null,
				iconEmoji: args.iconEmoji ?? null,
				sortOrder: state.sections.length,
				createdAt: '2026-05-10T00:00:00Z',
			};
			state.sections.push(section);
			return section;
		}),
		activitySectionsUpdate: vi.fn(async (args: Record<string, unknown>) => {
			const s = state.sections.find((s) => s.id === args.id);
			if (!s) throw new Error('section not found');
			if (args.label !== undefined) s.label = args.label;
			if (Object.hasOwn(args, 'iconLucide')) {
				s.iconLucide = args.iconLucide ?? null;
			}
			if (Object.hasOwn(args, 'iconEmoji')) {
				s.iconEmoji = args.iconEmoji ?? null;
			}
			return { ...s };
		}),
		activitySectionsRemove: vi.fn(async (id: string) => {
			state.sections = state.sections.filter((s) => s.id !== id);
			// SQL ON DELETE SET NULL — re-parent pins.
			for (const p of state.pins) {
				if (p.sectionId === id) p.sectionId = null;
			}
		}),
		__resetMockState: () => {
			state.pins = [];
			state.sections = [];
			state.nextId = 1;
		},
	};
});

import * as cmd from '@/lib/tauri-cmd';
import {
	computeCrossSectionReorderIds,
	computeReorderIds,
	dispatchPinSelection,
	fuzzyMatchSection,
	pinsOfPkg,
	pkgIdOfPin,
	prunePinsForUninstalledPkg,
	slugifySectionId,
	useActivityBarPins,
	usePinsStore,
	visiblePins,
	type PinDispatchTarget,
} from './pins-store';

const resetMockState = (cmd as unknown as { __resetMockState: () => void }).__resetMockState;

beforeEach(async () => {
	resetMockState();
	// Reset the store between tests (Zustand keeps a singleton).
	usePinsStore.setState({
		pins: [],
		sections: [],
		hydrated: false,
		loading: false,
		error: null,
	});
});

describe('slugifySectionId', () => {
	it('lowercases and replaces non-allowed characters', () => {
		expect(slugifySectionId('Finance')).toBe('finance');
		expect(slugifySectionId('My Section')).toBe('my-section');
		expect(slugifySectionId('  weird///chars!! ')).toBe('weird-chars');
	});

	it('strips leading and trailing dashes', () => {
		expect(slugifySectionId('--foo--')).toBe('foo');
	});
});

describe('fuzzyMatchSection', () => {
	const sections = [
		{
			id: 'finance',
			label: 'Finance',
			iconLucide: null,
			iconEmoji: null,
			sortOrder: 0,
			createdAt: 'x',
		},
		{
			id: 'design-tokens',
			label: 'Design Tokens',
			iconLucide: null,
			iconEmoji: null,
			sortOrder: 1,
			createdAt: 'x',
		},
	];

	it('matches on exact id', () => {
		expect(fuzzyMatchSection('finance', sections)?.id).toBe('finance');
	});

	it('matches on case-insensitive label', () => {
		expect(fuzzyMatchSection('FINANCE', sections)?.id).toBe('finance');
	});

	it('matches on substring of label', () => {
		expect(fuzzyMatchSection('design', sections)?.id).toBe('design-tokens');
	});

	it('returns null on miss', () => {
		expect(fuzzyMatchSection('outbound', sections)).toBeNull();
	});

	it('returns null on empty input', () => {
		expect(fuzzyMatchSection('   ', sections)).toBeNull();
	});
});

describe('pins store — section creation flow', () => {
	it('hydrates from empty disk', async () => {
		await usePinsStore.getState().hydrate();
		expect(usePinsStore.getState().pins).toEqual([]);
		expect(usePinsStore.getState().sections).toEqual([]);
		expect(usePinsStore.getState().hydrated).toBe(true);
	});

	it('creates a section, then pins to it', async () => {
		const store = usePinsStore.getState();
		await store.hydrate();
		const section = await store.createSection({
			id: 'finance',
			label: 'Finance',
			iconLucide: 'wallet',
		});
		expect(section.id).toBe('finance');
		expect(usePinsStore.getState().sections).toHaveLength(1);

		const pin = await usePinsStore.getState().addPin({
			kind: 'route',
			target: '/finance/expenses',
			label: 'Expenses',
			sectionId: 'finance',
		});
		expect(pin.sectionId).toBe('finance');
		expect(pin.sortOrder).toBe(0);
		expect(usePinsStore.getState().pins).toHaveLength(1);
	});

	it('rejects pinning to a non-existent section', async () => {
		const store = usePinsStore.getState();
		await store.hydrate();
		await expect(
			usePinsStore.getState().addPin({
				kind: 'route',
				target: '/x',
				label: 'X',
				sectionId: 'nope',
			})
		).rejects.toThrow();
	});

	it('rejects creating a section with a reserved id', async () => {
		const store = usePinsStore.getState();
		await store.hydrate();
		await expect(store.createSection({ id: 'system', label: 'System' })).rejects.toThrow();
		await expect(store.createSection({ id: 'settings', label: 'Settings' })).rejects.toThrow();
	});
});

describe('pins store — reorder', () => {
	it('reorders pins within a section and persists sort_order', async () => {
		const store = usePinsStore.getState();
		await store.hydrate();
		await store.createSection({ id: 'finance', label: 'Finance' });
		const a = await usePinsStore.getState().addPin({
			kind: 'route',
			target: '/a',
			label: 'A',
			sectionId: 'finance',
		});
		const b = await usePinsStore.getState().addPin({
			kind: 'route',
			target: '/b',
			label: 'B',
			sectionId: 'finance',
		});
		const c = await usePinsStore.getState().addPin({
			kind: 'route',
			target: '/c',
			label: 'C',
			sectionId: 'finance',
		});

		// Reverse the order: c, b, a.
		await usePinsStore.getState().reorderPins([c.id, b.id, a.id], 'finance');

		const after = usePinsStore.getState().pins;
		const orderById = new Map(after.map((p) => [p.id, p.sortOrder] as const));
		expect(orderById.get(c.id)).toBe(0);
		expect(orderById.get(b.id)).toBe(1);
		expect(orderById.get(a.id)).toBe(2);
	});

	it('moves a pin into a different section via reorder', async () => {
		const store = usePinsStore.getState();
		await store.hydrate();
		await store.createSection({ id: 'finance', label: 'Finance' });
		await store.createSection({ id: 'ops', label: 'Ops' });
		const a = await usePinsStore.getState().addPin({
			kind: 'route',
			target: '/a',
			label: 'A',
			sectionId: 'finance',
		});
		// Reorder into 'ops' moves the pin's sectionId.
		await usePinsStore.getState().reorderPins([a.id], 'ops');
		const moved = usePinsStore.getState().pins.find((p) => p.id === a.id);
		expect(moved?.sectionId).toBe('ops');
		expect(moved?.sortOrder).toBe(0);
	});

	it('reverts state on Rust error', async () => {
		const store = usePinsStore.getState();
		await store.hydrate();
		await store.createSection({ id: 'finance', label: 'Finance' });
		const a = await usePinsStore.getState().addPin({
			kind: 'route',
			target: '/a',
			label: 'A',
			sectionId: 'finance',
		});

		const reorderMock = cmd.activityPinsReorder as ReturnType<typeof vi.fn>;
		reorderMock.mockImplementationOnce(async () => {
			throw new Error('boom');
		});

		await expect(usePinsStore.getState().reorderPins([a.id], 'finance')).rejects.toThrow('boom');
		// State should be back to original (sortOrder still 0, sectionId
		// unchanged).
		const restored = usePinsStore.getState().pins.find((p) => p.id === a.id);
		expect(restored?.sectionId).toBe('finance');
		expect(restored?.sortOrder).toBe(0);
	});
});

describe('pins store — section removal re-parents pins', () => {
	it('sets sectionId to null on pins when their section is removed', async () => {
		const store = usePinsStore.getState();
		await store.hydrate();
		await store.createSection({ id: 'finance', label: 'Finance' });
		const a = await usePinsStore.getState().addPin({
			kind: 'route',
			target: '/a',
			label: 'A',
			sectionId: 'finance',
		});
		await usePinsStore.getState().removeSection('finance');
		const after = usePinsStore.getState().pins.find((p) => p.id === a.id);
		expect(after?.sectionId).toBeNull();
		expect(usePinsStore.getState().sections).toHaveLength(0);
	});
});

describe('dispatchPinSelection', () => {
	function makeStore(): PinDispatchTarget & {
		navigateFocused: ReturnType<typeof vi.fn>;
		placeView: ReturnType<typeof vi.fn>;
	} {
		return {
			focusedId: 'leaf-1',
			navigateFocused: vi.fn(),
			placeView: vi.fn(() => true),
		};
	}

	const basePin = {
		id: 'p',
		label: 'X',
		iconLucide: null,
		iconEmoji: null,
		sectionId: null,
		sortOrder: 0,
		createdAt: '2026-05-15T00:00:00Z',
		manifestId: null,
		lastOpenedAt: null,
	};

	it('navigates the focused pane for route pins', () => {
		const store = makeStore();
		dispatchPinSelection({ ...basePin, kind: 'route', target: '/inbox' }, store);
		expect(store.navigateFocused).toHaveBeenCalledWith('/inbox');
		expect(store.placeView).not.toHaveBeenCalled();
	});

	it('navigates the focused pane for pkg-route pins', () => {
		const store = makeStore();
		dispatchPinSelection({ ...basePin, kind: 'pkg-route', target: '/pkg/com.example/foo' }, store);
		expect(store.navigateFocused).toHaveBeenCalledWith('/pkg/com.example/foo');
		expect(store.placeView).not.toHaveBeenCalled();
	});

	it('places an artifact view for artifact pins', () => {
		const store = makeStore();
		dispatchPinSelection({ ...basePin, kind: 'artifact', target: '/home/me/cfo.html' }, store);
		expect(store.placeView).toHaveBeenCalledWith(
			'leaf-1',
			{ kind: 'artifact', path: '/home/me/cfo.html' },
			'append'
		);
		expect(store.navigateFocused).not.toHaveBeenCalled();
	});

	it('places an artifact view for file pins', () => {
		const store = makeStore();
		dispatchPinSelection({ ...basePin, kind: 'file', target: '/notes.md' }, store);
		expect(store.placeView).toHaveBeenCalledWith(
			'leaf-1',
			{ kind: 'artifact', path: '/notes.md' },
			'append'
		);
	});

	it('places an artifact view for external (URL) pins', () => {
		const store = makeStore();
		dispatchPinSelection(
			{ ...basePin, kind: 'external', target: 'https://example.com/dash' },
			store
		);
		expect(store.placeView).toHaveBeenCalledWith(
			'leaf-1',
			{ kind: 'artifact', path: 'https://example.com/dash' },
			'append'
		);
	});
});

describe('computeReorderIds (same-section drag)', () => {
	const list = (...ids: string[]) => ids.map((id) => ({ id }));

	it('moves the first item one slot down', () => {
		// dstIdx is the index AFTER removal. With [a,b,c], dragging a to "after b"
		// means: remove a -> [b,c]; insert a at idx 1 -> [b,a,c].
		expect(computeReorderIds(list('a', 'b', 'c'), 0, 1)).toEqual(['b', 'a', 'c']);
	});

	it('moves the last item to the front', () => {
		expect(computeReorderIds(list('a', 'b', 'c'), 2, 0)).toEqual(['c', 'a', 'b']);
	});

	it('handles dstIdx beyond the end (clamped to tail)', () => {
		expect(computeReorderIds(list('a', 'b', 'c'), 0, 99)).toEqual(['b', 'c', 'a']);
	});

	it('handles negative dstIdx (clamped to head)', () => {
		expect(computeReorderIds(list('a', 'b', 'c'), 2, -5)).toEqual(['c', 'a', 'b']);
	});

	it('returns [] for an out-of-range source index', () => {
		expect(computeReorderIds(list('a', 'b'), 5, 0)).toEqual([]);
		expect(computeReorderIds(list('a', 'b'), -1, 0)).toEqual([]);
	});

	it('returns the original order when srcIdx == dstIdx', () => {
		// Removing src then inserting at the same index reproduces the original.
		expect(computeReorderIds(list('a', 'b', 'c'), 1, 1)).toEqual(['a', 'b', 'c']);
	});
});

describe('computeCrossSectionReorderIds (cross-section drop)', () => {
	const list = (...ids: string[]) => ids.map((id) => ({ id }));

	it('inserts at the front of an empty destination', () => {
		expect(computeCrossSectionReorderIds(list(), 'x', 0)).toEqual(['x']);
	});

	it('inserts at the head of a populated destination', () => {
		expect(computeCrossSectionReorderIds(list('a', 'b'), 'x', 0)).toEqual(['x', 'a', 'b']);
	});

	it('inserts in the middle', () => {
		expect(computeCrossSectionReorderIds(list('a', 'b', 'c'), 'x', 2)).toEqual([
			'a',
			'b',
			'x',
			'c',
		]);
	});

	it('appends when dstIdx is at or past the tail', () => {
		expect(computeCrossSectionReorderIds(list('a', 'b'), 'x', 2)).toEqual(['a', 'b', 'x']);
		expect(computeCrossSectionReorderIds(list('a', 'b'), 'x', 99)).toEqual(['a', 'b', 'x']);
	});

	it('clamps a negative dstIdx to the front', () => {
		expect(computeCrossSectionReorderIds(list('a', 'b'), 'x', -1)).toEqual(['x', 'a', 'b']);
	});
});

describe('pins of an uninstalled pkg (pkg-uninstalled prune)', () => {
	const pin = (kind: string, target: string, manifestId: string | null = null) =>
		({ kind, target, manifestId }) as never;

	it('matches route / pkg-route pins into /pkg/<id>, and pin_on_install pins by manifestId', () => {
		const pins = [
			pin('route', '/pkg/com.x.studio/grid', 'com.x.studio'),
			pin('pkg-route', '/pkg/com.x.studio'),
			pin('route', '/pkg/com.x.studio/loupe?id=1'),
			pin('route', '/pkg/com.x.studio2/grid'),
			pin('route', '/settings'),
			pin('artifact', '/pkg/com.x.studio/page.html'),
		];
		expect(pinsOfPkg(pins, 'com.x.studio')).toEqual([pins[0], pins[1], pins[2]]);
	});

	it('prunes only the uninstalled pkg pins and re-reads the store', async () => {
		const { addPin } = usePinsStore.getState();
		await addPin({ kind: 'route', target: '/pkg/com.x.studio/grid', label: 'Grid', manifestId: 'com.x.studio' });
		await addPin({ kind: 'route', target: '/pkg/com.x.studio/loupe', label: 'Loupe' });
		await addPin({ kind: 'route', target: '/pkg/com.x.studio2/grid', label: 'Other' });
		await addPin({ kind: 'artifact', target: '/home/x/a.html', label: 'Doc' });

		const removed = await prunePinsForUninstalledPkg('com.x.studio');

		expect(removed).toHaveLength(2);
		expect(cmd.activityPinsRemove).toHaveBeenCalledTimes(2);
		const left = usePinsStore.getState().pins.map((p) => p.target);
		expect(left).toEqual(['/pkg/com.x.studio2/grid', '/home/x/a.html']);
	});

	it('is a no-op when the pkg had no pins', async () => {
		await usePinsStore.getState().addPin({ kind: 'route', target: '/settings', label: 'Settings' });
		vi.mocked(cmd.activityPinsRemove).mockClear();
		expect(await prunePinsForUninstalledPkg('com.x.none')).toEqual([]);
		expect(cmd.activityPinsRemove).not.toHaveBeenCalled();
	});
});

describe('pins into an unregistered pkg (on disk, failed to load)', () => {
	const pin = (kind: string, target: string) =>
		({ kind, target }) as { kind: 'route' | 'artifact' | 'pkg-route'; target: string };

	it('reads the pkg id off route / pkg-route targets only', () => {
		expect(pkgIdOfPin(pin('route', '/pkg/com.ikenga.meetings/meetings'))).toBe('com.ikenga.meetings');
		expect(pkgIdOfPin(pin('pkg-route', '/pkg/com.x.studio?tab=1'))).toBe('com.x.studio');
		expect(pkgIdOfPin(pin('route', '/pkg/com.x.studio'))).toBe('com.x.studio');
		expect(pkgIdOfPin(pin('route', '/settings'))).toBeNull();
		expect(pkgIdOfPin(pin('artifact', '/pkg/com.x.studio/page.html'))).toBeNull();
	});

	it('keeps everything while the kernel snapshot is unknown', () => {
		const pins = [pin('route', '/pkg/com.ikenga.meetings/meetings')];
		expect(visiblePins(pins, null)).toEqual(pins);
		expect(visiblePins(pins, undefined)).toEqual(pins);
	});

	it('hides a rail pin whose pkg is not registered, without deleting it, and shows it once it registers', async () => {
		const { addPin } = usePinsStore.getState();
		await addPin({ kind: 'route', target: '/pkg/com.ikenga.meetings/meetings', label: 'Meetings' });
		await addPin({ kind: 'route', target: '/pkg/com.ikenga.studio/grid', label: 'Studio' });
		await addPin({ kind: 'route', target: '/settings', label: 'Settings' });
		vi.mocked(cmd.activityPinsRemove).mockClear();

		const { result, rerender } = renderHook(
			({ available }: { available: ReadonlySet<string> | null }) => useActivityBarPins(available),
			{ initialProps: { available: new Set(['com.ikenga.studio']) as ReadonlySet<string> | null } }
		);
		const labels = () => result.current.sectionLessPins.map((p) => p.label);

		expect(labels()).toEqual(['Studio', 'Settings']);
		// Hidden, not pruned: the stored pin is untouched.
		expect(cmd.activityPinsRemove).not.toHaveBeenCalled();
		expect(usePinsStore.getState().pins.map((p) => p.label)).toContain('Meetings');

		// The pkg is repaired / reinstalled → its view registers → the pin returns.
		rerender({ available: new Set(['com.ikenga.studio', 'com.ikenga.meetings']) });
		expect(labels()).toEqual(['Meetings', 'Studio', 'Settings']);
	});
});
