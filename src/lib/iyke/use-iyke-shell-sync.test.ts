// WP-21: `useIykeShellSync` pushes the store's derived `activeProject` into
// the Rust mirror behind `iyke state` (`shell.active_project`), and the keymap
// registry behind `GET /iyke/keys` exactly once when the workspace mounts.

import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { DEFAULT_KEYMAP } from '@/lib/keymap/defaults';
import { formatKeyLabel } from '@/lib/keymap/platform';
import { useShellStore } from '@/lib/shell/shell-store';
import type { IykeKeymapEntry } from '@/lib/tauri-cmd';

const iykeSetFrame = vi.fn((_args: unknown) => Promise.resolve());
const setShell = vi.fn((_args: unknown) => Promise.resolve());

vi.mock('@/lib/tauri-cmd', () => ({
	iykeSetFrame: (args: unknown) => iykeSetFrame(args),
}));
vi.mock('./client', () => ({ setShell: (args: unknown) => setShell(args) }));

import { keymapPayload, useIykeShellSync } from './use-iyke-shell-sync';

type FrameArgs = {
	activeProject?: { id: string; root_path: string | null; extra_roots: string[] } | null;
	keymap?: IykeKeymapEntry[] | null;
};

function frameCalls(): FrameArgs[] {
	return iykeSetFrame.mock.calls.map((c) => c[0] as FrameArgs);
}
const keymapPushes = () => frameCalls().filter((a) => a.keymap);
const projectPushes = () => frameCalls().filter((a) => a.activeProject);

describe('useIykeShellSync — WP-21 frame push', () => {
	beforeEach(() => {
		iykeSetFrame.mockClear();
		setShell.mockClear();
		useShellStore.setState({
			activeProjectId: 'default',
			projectExtraRoots: {},
			carriedRoots: [],
			activeProject: { id: 'default', root_path: null, extra_roots: [] },
		});
	});
	afterEach(() => {
		vi.restoreAllMocks();
	});

	it('pushes active_project matching the store, and re-pushes when it changes', () => {
		renderHook(() => useIykeShellSync());

		expect(projectPushes()).toHaveLength(1);
		expect(projectPushes()[0].activeProject).toEqual(useShellStore.getState().activeProject);
		expect(projectPushes()[0].activeProject).toEqual({
			id: 'default',
			root_path: null,
			extra_roots: [],
		});

		act(() => {
			useShellStore.getState().setProjectExtraRoots('default', ['/work/a', ' /work/b ', '/work/a']);
		});

		const store = useShellStore.getState().activeProject;
		expect(store.extra_roots).toEqual(['/work/a', '/work/b']);
		expect(projectPushes()).toHaveLength(2);
		expect(projectPushes()[1].activeProject).toEqual(store);
		// Pushes carry only the field they're about.
		expect(projectPushes()[1].keymap).toBeUndefined();

		act(() => {
			useShellStore.setState({
				activeProjectId: 'p1',
				activeProject: { id: 'p1', root_path: '/work/p1', extra_roots: ['/x'] },
			});
		});
		expect(projectPushes()).toHaveLength(3);
		expect(projectPushes()[2].activeProject).toEqual({
			id: 'p1',
			root_path: '/work/p1',
			extra_roots: ['/x'],
		});
	});

	it('does not re-push active_project on unrelated store changes', () => {
		renderHook(() => useIykeShellSync());
		const before = projectPushes().length;
		act(() => {
			useShellStore.setState({ sidebarCollapsed: !useShellStore.getState().sidebarCollapsed });
		});
		expect(projectPushes()).toHaveLength(before);
	});

	it('pushes the full keymap registry exactly once at mount', () => {
		const { rerender } = renderHook(() => useIykeShellSync());

		expect(keymapPushes()).toHaveLength(1);
		const pushed = keymapPushes()[0].keymap!;
		expect(pushed).toHaveLength(DEFAULT_KEYMAP.length);
		expect(pushed.map((e) => e.command)).toEqual(DEFAULT_KEYMAP.map((e) => e.command));
		for (const [i, e] of DEFAULT_KEYMAP.entries()) {
			expect(pushed[i]).toMatchObject({
				command: e.command,
				key: e.key,
				when: e.when,
				source: e.source,
				label: e.label,
				key_label: formatKeyLabel(e.key),
			});
			expect(pushed[i].platform_only).toBe(e.platformOnly);
		}
		// The keymap push carries no active_project.
		expect(keymapPushes()[0].activeProject).toBeUndefined();

		// Re-renders and store churn never re-push it.
		rerender();
		act(() => {
			useShellStore.getState().setProjectExtraRoots('default', ['/work/c']);
			useShellStore.setState({ sidebarCollapsed: !useShellStore.getState().sidebarCollapsed });
		});
		rerender();
		expect(keymapPushes()).toHaveLength(1);
	});

	it('keymapPayload omits platform_only when the entry has none', () => {
		const [row] = keymapPayload([
			{ command: 'x.y', key: 'mod+k', when: 'global', source: 'default', label: 'X' },
		]);
		expect(row).not.toHaveProperty('platform_only');
		expect(row.key_label).toBe(formatKeyLabel('mod+k'));
	});

	it('logs, never throws, when the push fails', async () => {
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
		iykeSetFrame.mockImplementation(() => Promise.reject(new Error('boom')));
		renderHook(() => useIykeShellSync());
		await act(async () => {
			await Promise.resolve();
		});
		expect(warn.mock.calls.some((c) => String(c[0]).includes('set_frame'))).toBe(true);
		iykeSetFrame.mockImplementation(() => Promise.resolve());
	});
});
