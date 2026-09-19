// theme-store workspace union (WP-03): the only writer of <html data-workspace>
// now carries the four rail nouns; a v1 blob's pre-v16 name is migrated.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// Each test re-imports the store (vi.resetModules) so it rehydrates from the
// localStorage it seeded; the first cold import is slow under a full run.
vi.setConfig({ testTimeout: 60_000 });

type ThemeStoreModule = typeof import('./theme-store');

async function freshStore(): Promise<ThemeStoreModule> {
	vi.resetModules();
	return import('./theme-store');
}

function seedV1(workspace: string) {
	localStorage.setItem(
		'ikenga.theme',
		JSON.stringify({
			state: { theme: 'B', mode: 'light', density: 'compact', tintStrength: 'strong', workspace },
			version: 1,
		})
	);
}

beforeEach(() => localStorage.clear());
afterEach(() => localStorage.clear());

describe('theme-store workspace', () => {
	it('defaults to project on a fresh profile', async () => {
		const { useIkengaStore, IKENGA_WORKSPACES } = await freshStore();
		expect(useIkengaStore.getState().workspace).toBe('project');
		expect(IKENGA_WORKSPACES).toEqual(['project', 'chi', 'ngwa', 'settings']);
	});

	it.each([
		['app', 'project'],
		['files', 'project'],
		['sessions', 'project'],
		['artifact-grid', 'project'],
		['pkgs', 'ngwa'],
		['ngwa', 'ngwa'],
		['settings', 'settings'],
		['garbage', 'project'],
	])('migrates a v1 workspace %s → %s and keeps the other prefs', async (from, to) => {
		seedV1(from);
		const { useIkengaStore } = await freshStore();
		const s = useIkengaStore.getState();
		expect(s.workspace).toBe(to);
		expect([s.theme, s.mode, s.density, s.tintStrength]).toEqual([
			'B',
			'light',
			'compact',
			'strong',
		]);
	});

	it('writes the workspace to <html data-workspace>', async () => {
		const { useIkengaStore, installIkengaDomSync } = await freshStore();
		installIkengaDomSync();
		for (const w of ['chi', 'ngwa', 'settings', 'project'] as const) {
			useIkengaStore.getState().setWorkspace(w);
			expect(document.documentElement.getAttribute('data-workspace')).toBe(w);
		}
	});
});
