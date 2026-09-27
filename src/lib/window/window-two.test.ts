// WP-69 — Pop out joins Window 2 (G-SEATS §4.4, DEC-69d, pin P-7).
import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@/lib/tauri-cmd', () => ({
	listWindows: vi.fn(async () => []),
	spawnWindow: vi.fn(async (d: { label: string }) => d.label),
	windowJoinSurface: vi.fn(async (): Promise<string | null> => null),
	windowRemoveSurface: vi.fn(async () => [] as string[]),
}));
vi.mock('./window-context', () => ({ isDetachedWindow: () => false }));

import { listWindows, spawnWindow, windowJoinSurface, windowRemoveSurface } from '@/lib/tauri-cmd';
import { useDetachedSurfaces } from './detached-surfaces';
import { PENDING_WINDOW_LABEL } from './surfaces-topic';
import { moveSurfaceBack, newDetachedLabel, popOutSurface } from './window-two';

const join = vi.mocked(windowJoinSurface);
const spawn = vi.mocked(spawnWindow);

beforeEach(() => {
	useDetachedSurfaces.setState({ surfaceToWindow: {} });
	join.mockReset().mockResolvedValue(null);
	spawn.mockReset().mockImplementation(async (d) => d.label);
	vi.mocked(listWindows).mockReset().mockResolvedValue([]);
});

describe('popOutSurface', () => {
	it('joins Window 2 when one is open, and spawns nothing', async () => {
		join.mockResolvedValue('detached-terminal-w2');
		const r = await popOutSurface('terminal:p1', { projectId: 'royalti-co' });
		expect(join).toHaveBeenCalledWith('terminal:p1', 'royalti-co');
		expect(spawn).not.toHaveBeenCalled();
		expect(r).toEqual({ label: 'detached-terminal-w2', joined: true });
		expect(useDetachedSurfaces.getState().surfaceToWindow['terminal:p1']).toBe('detached-terminal-w2');
	});

	it('spawns a single-surface window only when there is no Window 2', async () => {
		const r = await popOutSurface('terminal:p1', { projectId: null });
		expect(spawn).toHaveBeenCalledTimes(1);
		const d = spawn.mock.calls[0][0];
		expect(d).toMatchObject({ kind: 'single-surface', surface_set: ['terminal:p1'], project_id: null });
		expect(d.label).toMatch(/^detached-terminal-/);
		expect(d.layout_key).toBe(d.label);
		expect(r).toEqual({ label: d.label, joined: false });
		expect(useDetachedSurfaces.getState().surfaceToWindow['terminal:p1']).toBe(d.label);
	});

	it('marks the surface detached (provisionally) before the IPC resolves', async () => {
		let release: (v: string | null) => void = () => {};
		join.mockImplementation(() => new Promise((r) => (release = r)));
		const p = popOutSurface('terminal:p1', { projectId: null });
		expect(useDetachedSurfaces.getState().surfaceToWindow['terminal:p1']).toBe(PENDING_WINDOW_LABEL);
		release('detached-w2');
		await p;
		expect(useDetachedSurfaces.getState().surfaceToWindow['terminal:p1']).toBe('detached-w2');
	});

	it('un-marks the surface and rethrows when the pop-out fails', async () => {
		join.mockRejectedValue(new Error('ipc down'));
		await expect(popOutSurface('terminal:p1', { projectId: null })).rejects.toThrow('ipc down');
		expect('terminal:p1' in useDetachedSurfaces.getState().surfaceToWindow).toBe(false);
	});
});

describe('moveSurfaceBack', () => {
	it('removes the surface from its window as a move back', async () => {
		await moveSurfaceBack('detached-w2', 'terminal:p1');
		expect(windowRemoveSurface).toHaveBeenCalledWith('detached-w2', 'terminal:p1', true);
	});
});

it('new labels match the detached capability glob', () => {
	expect(newDetachedLabel()).toMatch(/^detached-terminal-[a-z0-9]+-[a-z0-9]+$/);
	expect(newDetachedLabel('viewer')).toMatch(/^detached-viewer-/);
});
