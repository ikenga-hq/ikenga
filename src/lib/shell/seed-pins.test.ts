import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { ActivityPin } from '@/lib/tauri-cmd';
import type { PkgActivityBarEntry } from '@/lib/pkg/use-activity-bar-entries';

const settingsGetAll = vi.fn();
const settingsSet = vi.fn();
const pkgKernelStatus = vi.fn();
const activityPinsList = vi.fn();
const activityPinsAdd = vi.fn();

vi.mock('@/lib/tauri-cmd', () => ({
	settingsGetAll: (...args: unknown[]) => settingsGetAll(...args),
	settingsSet: (...args: unknown[]) => settingsSet(...args),
	pkgKernelStatus: (...args: unknown[]) => pkgKernelStatus(...args),
	activityPinsList: (...args: unknown[]) => activityPinsList(...args),
	activityPinsAdd: (...args: unknown[]) => activityPinsAdd(...args),
}));

import { computeEntriesToSeed, pinLabelFor, seedPinsFromRail, SEED_PINS_FLAG } from './seed-pins';

function makeEntry(overrides: Partial<PkgActivityBarEntry> = {}): PkgActivityBarEntry {
	return {
		pkg_id: 'com.ikenga.tasks',
		pkg_name: 'Tasks',
		id: 'tasks',
		label: 'Tasks',
		icon: 'list-check',
		section: null,
		route: '/pkg/com.ikenga.tasks',
		nav: [],
		...overrides,
	};
}

function makePin(overrides: Partial<ActivityPin> = {}): ActivityPin {
	return {
		id: 'pin-1',
		kind: 'route',
		target: '/pkg/com.ikenga.tasks',
		label: 'Tasks',
		iconLucide: 'list-check',
		iconEmoji: null,
		sectionId: null,
		sortOrder: 0,
		createdAt: '2026-01-01T00:00:00.000Z',
		manifestId: null,
		lastOpenedAt: null,
		...overrides,
	};
}

beforeEach(() => {
	vi.clearAllMocks();
});

describe('computeEntriesToSeed', () => {
	it('excludes entries already pinned by route target, keeps registry order', () => {
		const entries = [
			makeEntry({ pkg_id: 'a', route: '/pkg/a' }),
			makeEntry({ pkg_id: 'b', route: '/pkg/b' }),
			makeEntry({ pkg_id: 'c', route: '/pkg/c' }),
		];
		const pins = [makePin({ id: 'p-b', target: '/pkg/b' })];
		const result = computeEntriesToSeed(entries, pins);
		expect(result.map((e) => e.route)).toEqual(['/pkg/a', '/pkg/c']);
	});

	it('excludes entries already pinned by manifestId match on pkg_id', () => {
		const entries = [makeEntry({ pkg_id: 'com.ikenga.sales', route: '/pkg/sales' })];
		const pins = [makePin({ target: '/other', manifestId: 'com.ikenga.sales' })];
		expect(computeEntriesToSeed(entries, pins)).toEqual([]);
	});

	it('does not exclude a non-route pin with the same target', () => {
		const entries = [makeEntry({ route: '/pkg/a' })];
		const pins = [makePin({ kind: 'artifact', target: '/pkg/a', manifestId: null })];
		expect(computeEntriesToSeed(entries, pins)).toHaveLength(1);
	});
});

describe('seedPinsFromRail', () => {
	it('D1: seeds every unpinned registry entry, skips overlapping ones, no duplicates, stable order', async () => {
		settingsGetAll.mockResolvedValue({});
		const entries = [
			makeEntry({ pkg_id: 'a', route: '/pkg/a', label: 'A' }),
			makeEntry({ pkg_id: 'b', route: '/pkg/b', label: 'B' }),
			makeEntry({ pkg_id: 'c', route: '/pkg/c', label: 'C' }),
		];
		pkgKernelStatus.mockResolvedValue({
			registries: { activity_bar: { entries } },
			installed: [],
			api_version: 1,
		});
		activityPinsList.mockResolvedValue([makePin({ id: 'existing-b', target: '/pkg/b' })]);
		activityPinsAdd.mockImplementation(async (args) => ({
			...makePin(),
			...args,
			id: `new-${args.target}`,
		}));

		await seedPinsFromRail();

		expect(activityPinsAdd).toHaveBeenCalledTimes(2);
		expect(activityPinsAdd.mock.calls.map((c) => c[0].target)).toEqual(['/pkg/a', '/pkg/c']);
		expect(settingsSet).toHaveBeenCalledWith(SEED_PINS_FLAG, JSON.stringify(true));
	});

	it('D2: second boot with the flag set adds nothing', async () => {
		settingsGetAll.mockResolvedValue({ [SEED_PINS_FLAG]: JSON.stringify(true) });

		await seedPinsFromRail();

		expect(pkgKernelStatus).not.toHaveBeenCalled();
		expect(activityPinsList).not.toHaveBeenCalled();
		expect(activityPinsAdd).not.toHaveBeenCalled();
		expect(settingsSet).not.toHaveBeenCalled();
	});

	it('D3: unpin then reboot stays unpinned (stateful round trip across two boots)', async () => {
		// In-memory fakes so boot 2 actually reads what boot 1 wrote, instead
		// of each boot being independently mocked.
		const kv: Record<string, string> = {};
		let pins: ActivityPin[] = [];
		const entries = [makeEntry({ pkg_id: 'a', route: '/pkg/a', label: 'A' })];

		settingsGetAll.mockImplementation(async () => ({ ...kv }));
		settingsSet.mockImplementation(async (key: string, value: string) => {
			kv[key] = value;
		});
		pkgKernelStatus.mockResolvedValue({
			registries: { activity_bar: { entries } },
			installed: [],
			api_version: 1,
		});
		activityPinsList.mockImplementation(async () => pins);
		activityPinsAdd.mockImplementation(async (args: Partial<ActivityPin>) => {
			const pin = { ...makePin(), ...args, id: `new-${args.target}` } as ActivityPin;
			pins = [...pins, pin];
			return pin;
		});

		// Boot 1: seeds the one registry entry and sets the flag.
		await seedPinsFromRail();
		expect(activityPinsAdd).toHaveBeenCalledTimes(1);
		expect(pins.some((p) => p.target === '/pkg/a')).toBe(true);
		expect(kv[SEED_PINS_FLAG]).toBe(JSON.stringify(true));

		// The user unpins it.
		pins = pins.filter((p) => p.target !== '/pkg/a');

		// Boot 2: reads the same flag boot 1 wrote — must not re-seed.
		activityPinsAdd.mockClear();
		await seedPinsFromRail();

		expect(activityPinsAdd).not.toHaveBeenCalled();
		expect(pins.some((p) => p.target === '/pkg/a')).toBe(false);
	});

	it('D4: a failed activityPinsAdd does not set the flag and does not throw into boot', async () => {
		settingsGetAll.mockResolvedValue({});
		const entries = [makeEntry({ pkg_id: 'a', route: '/pkg/a' })];
		pkgKernelStatus.mockResolvedValue({
			registries: { activity_bar: { entries } },
			installed: [],
			api_version: 1,
		});
		activityPinsList.mockResolvedValue([]);
		activityPinsAdd.mockRejectedValue(new Error('rust said no'));

		await expect(seedPinsFromRail()).resolves.toBeUndefined();

		expect(settingsSet).not.toHaveBeenCalled();
	});

	it('D4: a failed kernel snapshot does not set the flag and does not throw into boot', async () => {
		settingsGetAll.mockResolvedValue({});
		pkgKernelStatus.mockRejectedValue(new Error('no tauri runtime'));

		await expect(seedPinsFromRail()).resolves.toBeUndefined();

		expect(activityPinsAdd).not.toHaveBeenCalled();
		expect(settingsSet).not.toHaveBeenCalled();
	});

	it('logs a one-line seed count on success', async () => {
		const infoSpy = vi.spyOn(console, 'info').mockImplementation(() => {});
		settingsGetAll.mockResolvedValue({});
		const entries = [makeEntry({ pkg_id: 'a', route: '/pkg/a' })];
		pkgKernelStatus.mockResolvedValue({
			registries: { activity_bar: { entries } },
			installed: [],
			api_version: 1,
		});
		activityPinsList.mockResolvedValue([]);
		activityPinsAdd.mockResolvedValue(makePin());

		await seedPinsFromRail();

		expect(infoSpy).toHaveBeenCalledWith(expect.stringContaining('seeded 1 pins'));
		infoSpy.mockRestore();
	});

	it('logs "already seeded" when the flag is set', async () => {
		const infoSpy = vi.spyOn(console, 'info').mockImplementation(() => {});
		settingsGetAll.mockResolvedValue({ [SEED_PINS_FLAG]: JSON.stringify(true) });

		await seedPinsFromRail();

		expect(infoSpy).toHaveBeenCalledWith(expect.stringContaining('already seeded'));
		infoSpy.mockRestore();
	});
});

describe('pinLabelFor', () => {
	it('uses the nav label, not the section-derived rail label', () => {
		const entry = makeEntry({
			pkg_name: 'Wikipedia',
			label: 'apps',
			section: 'apps',
			nav: [
				{
					id: 'wikipedia',
					label: 'Wikipedia',
					section: 'apps',
					route: '/pkg/com.ikenga.wikipedia/',
				},
			],
		});
		expect(pinLabelFor(entry)).toBe('Wikipedia');
	});

	it('falls back to the pkg name, then the rail label', () => {
		expect(pinLabelFor(makeEntry({ pkg_name: 'Sentry', label: 'apps', nav: [] }))).toBe('Sentry');
		expect(pinLabelFor(makeEntry({ pkg_name: '', label: 'Notion', nav: [] }))).toBe('Notion');
	});

	it('seeds pins with the nav label', async () => {
		settingsGetAll.mockResolvedValue({});
		activityPinsList.mockResolvedValue([]);
		pkgKernelStatus.mockResolvedValue({
			registries: {
				activity_bar: {
					entries: [
						makeEntry({
							pkg_id: 'com.ikenga.notion',
							pkg_name: 'Notion',
							label: 'apps',
							route: '/pkg/com.ikenga.notion/',
							nav: [
								{
									id: 'notion',
									label: 'Notion',
									section: 'apps',
									route: '/pkg/com.ikenga.notion/',
								},
							],
						}),
					],
				},
			},
		});
		activityPinsAdd.mockResolvedValue(makePin());
		settingsSet.mockResolvedValue(undefined);
		await seedPinsFromRail();
		expect(activityPinsAdd).toHaveBeenCalledWith(expect.objectContaining({ label: 'Notion' }));
	});
});
