// Gap audit rank 25 — the iyke shell-state mirror (`iyke_set_frame`,
// `iyke_set_actions_frame`, `iyke_set_shell`, …) is the desktop app's
// localhost bridge; the headless daemon serves none of it, so a browser
// session fired a failing RPC on every keymap / explorer / project / actions
// change. `useWorkspaceEffects` now mounts the sync only on the desktop.

import { cleanup, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	iykeSetFrame: vi.fn(async (_args: unknown) => {}),
	iykeSetActionsFrame: vi.fn(async (_args: unknown) => {}),
	setShell: vi.fn(async (_args: unknown) => {}),
}));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	iykeSetFrame: h.iykeSetFrame,
	iykeSetActionsFrame: h.iykeSetActionsFrame,
	iykeActionsRequestDone: vi.fn(async () => {}),
	listen: vi.fn(async () => () => {}),
}));
vi.mock('@/lib/iyke/client', async (orig) => ({
	...(await orig<typeof import('@/lib/iyke/client')>()),
	setShell: h.setShell,
}));

import { pickIykeShellSync } from './workspace-effects';

beforeEach(() => {
	h.iykeSetFrame.mockClear();
	h.iykeSetActionsFrame.mockClear();
	h.setShell.mockClear();
});
afterEach(() => cleanup());

describe('workspace iyke shell-sync (gap rank 25)', () => {
	it('is a no-op outside the desktop app — no iyke_set_frame / iyke_set_shell', () => {
		renderHook(() => pickIykeShellSync(false)());
		expect(h.iykeSetFrame).not.toHaveBeenCalled();
		expect(h.iykeSetActionsFrame).not.toHaveBeenCalled();
		expect(h.setShell).not.toHaveBeenCalled();
	});

	it('still mirrors shell state on the desktop', () => {
		renderHook(() => pickIykeShellSync(true)());
		expect(h.iykeSetFrame).toHaveBeenCalled();
	});

	it('picks the no-op in a test/browser page (isTauri() is false)', () => {
		renderHook(() => pickIykeShellSync()());
		expect(h.iykeSetFrame).not.toHaveBeenCalled();
	});
});
