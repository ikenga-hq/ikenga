import { beforeEach, describe, expect, it, vi } from 'vitest';

// Mock the Tauri seams the store talks to. `listen` resolves to a no-op
// unlisten; `listWindows` / `windowRemoveSurface` are controllable per-test.
// WP-69: a reclaim takes ONE surface out of its window (Rust closes the
// window only when that was its last surface), so it calls
// `windowRemoveSurface`, not `closeWindow`.
vi.mock('@tauri-apps/api/event', () => ({
	listen: vi.fn(() => Promise.resolve(() => {})),
}));
vi.mock('@/lib/tauri-cmd', () => ({
	listWindows: vi.fn(),
	windowRemoveSurface: vi.fn(() => Promise.resolve([])),
}));
// Primary window by default (the tracker no-ops in a detached window).
vi.mock('./window-context', () => ({ isDetachedWindow: () => false }));

import type { WindowDescriptor } from '@ikenga/contract';
import { listWindows, windowRemoveSurface } from '@/lib/tauri-cmd';
import {
	clearPendingReclaimNudge,
	clearPendingSurface,
	handleSurfacesChanged,
	hasPendingReclaimNudge,
	markSurfaceDetached,
	onSurfacesReturned,
	reclaimSurface,
	type SurfacesReturned,
	syncDetachedSurfaces,
	useDetachedSurfaces,
	windowSurfaces,
} from './detached-surfaces';
import { PENDING_WINDOW_LABEL } from './surfaces-topic';

const mockListWindows = vi.mocked(listWindows);
const mockCloseWindow = vi.mocked(windowRemoveSurface);

function descriptor(label: string, surfaces: string[]): WindowDescriptor {
	return {
		label,
		kind: 'single-surface',
		surface_set: surfaces,
		project_id: null,
		layout_key: label,
	};
}

function isDetached(surfaceId: string): boolean {
	return surfaceId in useDetachedSurfaces.getState().surfaceToWindow;
}

// The `pendingReclaimNudge` Set is module-scope and survives between tests, so
// clear every id the suite touches up front — otherwise an arm in one test
// leaks into the next.
const NUDGE_IDS = ['terminal:pty-1', 'terminal:pty-9', 'viewer:/b.md', 'viewer:/a.md'];

beforeEach(() => {
	useDetachedSurfaces.setState({ surfaceToWindow: {} });
	for (const id of NUDGE_IDS) clearPendingReclaimNudge(id);
	mockListWindows.mockReset();
	mockCloseWindow.mockReset();
	mockCloseWindow.mockResolvedValue([]);
});

describe('syncDetachedSurfaces', () => {
	it('maps every detached window surface to its hosting label, skipping main', async () => {
		mockListWindows.mockResolvedValue([
			descriptor('detached-viewer-1', ['viewer:/b.md']),
			descriptor('detached-terminal-1', ['terminal:pty-9']),
			// `main` (if ever listed) must never count as detached.
			descriptor('main', ['viewer:/b.md']),
		]);

		await syncDetachedSurfaces();

		expect(isDetached('viewer:/b.md')).toBe(true);
		expect(isDetached('terminal:pty-9')).toBe(true);
		expect(useDetachedSurfaces.getState().surfaceToWindow['viewer:/b.md']).toBe(
			'detached-viewer-1'
		);
		expect(isDetached('viewer:/no/such')).toBe(false);
	});

	it('drops surfaces whose windows have closed (full rebuild, not merge)', async () => {
		markSurfaceDetached('viewer:/b.md', 'detached-viewer-1');
		expect(isDetached('viewer:/b.md')).toBe(true);

		// The window closed → registry no longer lists it.
		mockListWindows.mockResolvedValue([]);
		await syncDetachedSurfaces();

		expect(isDetached('viewer:/b.md')).toBe(false);
	});

	it('preserves prior state when the registry list call rejects', async () => {
		markSurfaceDetached('viewer:/a.md', 'detached-viewer-1');
		mockListWindows.mockRejectedValue(new Error('ipc down'));

		await syncDetachedSurfaces();

		expect(isDetached('viewer:/a.md')).toBe(true);
	});
});

describe('markSurfaceDetached', () => {
	it('optimistically records a surface as detached before the event lands', () => {
		expect(isDetached('terminal:pty-1')).toBe(false);
		markSurfaceDetached('terminal:pty-1', 'detached-terminal-1');
		expect(isDetached('terminal:pty-1')).toBe(true);
	});
});

describe('reclaimSurface', () => {
	it('takes the surface out of its hosting window and clears it from the map', async () => {
		markSurfaceDetached('viewer:/b.md', 'detached-viewer-1');

		await reclaimSurface('viewer:/b.md');

		expect(mockCloseWindow).toHaveBeenCalledWith('detached-viewer-1', 'viewer:/b.md');
		expect(isDetached('viewer:/b.md')).toBe(false);
	});

	it('WP-69: reclaiming one tab of a two-surface window leaves the other tab there', async () => {
		markSurfaceDetached('terminal:pty-1', 'detached-w2');
		markSurfaceDetached('terminal:pty-9', 'detached-w2');

		await reclaimSurface('terminal:pty-1');

		expect(mockCloseWindow).toHaveBeenCalledWith('detached-w2', 'terminal:pty-1');
		expect(isDetached('terminal:pty-1')).toBe(false);
		expect(windowSurfaces('detached-w2')).toEqual(['terminal:pty-9']);
	});

	it('WP-69: no-ops while the surface’s Pop out is still resolving', async () => {
		markSurfaceDetached('terminal:pty-1', PENDING_WINDOW_LABEL);
		await reclaimSurface('terminal:pty-1');
		expect(mockCloseWindow).not.toHaveBeenCalled();
		expect(isDetached('terminal:pty-1')).toBe(true);
	});

	it('no-ops when the surface is not detached', async () => {
		await reclaimSurface('viewer:/not-open');
		expect(mockCloseWindow).not.toHaveBeenCalled();
	});

	it('reconciles from the registry if the close call fails', async () => {
		markSurfaceDetached('viewer:/b.md', 'detached-viewer-1');
		mockCloseWindow.mockRejectedValue(new Error('close failed'));
		// The window genuinely closed despite the error → registry lists none.
		mockListWindows.mockResolvedValue([]);

		await reclaimSurface('viewer:/b.md');

		expect(isDetached('viewer:/b.md')).toBe(false);
	});
});

// T-3a (reclaim half of T-2): a reclaim arms a one-shot SIGWINCH nudge that
// TerminalView consumes on remount. Only `terminal:` surfaces are armed —
// TerminalView is the sole consumer and therefore the sole caller of
// `clearPendingReclaimNudge`, so arming a viewer surface would leak a Set
// slot that nothing ever clears.
describe('pendingReclaimNudge (T-3a reclaim arming)', () => {
	it('arms the nudge when a terminal surface is reclaimed via the button', async () => {
		markSurfaceDetached('terminal:pty-1', 'detached-terminal-1');
		expect(hasPendingReclaimNudge('terminal:pty-1')).toBe(false);

		await reclaimSurface('terminal:pty-1');

		expect(hasPendingReclaimNudge('terminal:pty-1')).toBe(true);
	});

	it('does NOT arm the nudge for a non-terminal (viewer) reclaim', async () => {
		markSurfaceDetached('viewer:/b.md', 'detached-viewer-1');

		await reclaimSurface('viewer:/b.md');

		// Nothing would ever clear it — must never be armed in the first place.
		expect(hasPendingReclaimNudge('viewer:/b.md')).toBe(false);
	});

	it('undoes the optimistic arm when the window close fails', async () => {
		markSurfaceDetached('terminal:pty-1', 'detached-terminal-1');
		mockCloseWindow.mockRejectedValue(new Error('close failed'));
		mockListWindows.mockResolvedValue([
			// The window is still open — the close genuinely didn't happen.
			descriptor('detached-terminal-1', ['terminal:pty-1']),
		]);

		await reclaimSurface('terminal:pty-1');

		// A future genuine reclaim, not this failed one, must be what arms it.
		expect(hasPendingReclaimNudge('terminal:pty-1')).toBe(false);
	});

	it('arms a terminal surface reclaimed via OS titlebar close (sync map-diff)', async () => {
		// Both were detached; the terminal window is closed via the OS chrome so
		// only the map-diff in syncDetachedSurfaces sees the transition.
		markSurfaceDetached('terminal:pty-9', 'detached-terminal-1');
		markSurfaceDetached('viewer:/b.md', 'detached-viewer-1');
		mockListWindows.mockResolvedValue([
			// terminal window gone; viewer window still open.
			descriptor('detached-viewer-1', ['viewer:/b.md']),
		]);

		await syncDetachedSurfaces();

		expect(hasPendingReclaimNudge('terminal:pty-9')).toBe(true);
		expect(isDetached('terminal:pty-9')).toBe(false);
	});

	it('does NOT arm a non-terminal surface closed via the OS titlebar', async () => {
		markSurfaceDetached('viewer:/b.md', 'detached-viewer-1');
		mockListWindows.mockResolvedValue([]);

		await syncDetachedSurfaces();

		expect(hasPendingReclaimNudge('viewer:/b.md')).toBe(false);
	});

	it('clearPendingReclaimNudge is idempotent — a double-clear is safe', () => {
		markSurfaceDetached('terminal:pty-1', 'detached-terminal-1');
		return reclaimSurface('terminal:pty-1').then(() => {
			expect(hasPendingReclaimNudge('terminal:pty-1')).toBe(true);
			clearPendingReclaimNudge('terminal:pty-1');
			expect(hasPendingReclaimNudge('terminal:pty-1')).toBe(false);
			// Second clear must not throw and must leave it cleared — this is the
			// StrictMode double-invoke path the consumer relies on.
			clearPendingReclaimNudge('terminal:pty-1');
			expect(hasPendingReclaimNudge('terminal:pty-1')).toBe(false);
		});
	});
});

// WP-69 (G-SEATS §4.4, DEC-69d): a detached window holds several surfaces as
// tabs; `window://surfaces-changed` carries joins and move-backs.
describe('multi-surface windows (WP-69)', () => {
	it('a join adds the surface to the window’s set; both point at one label', () => {
		markSurfaceDetached('terminal:pty-9', 'detached-w2');
		markSurfaceDetached('terminal:pty-1', PENDING_WINDOW_LABEL);

		handleSurfacesChanged({
			label: 'detached-w2',
			surface_set: ['terminal:pty-9', 'terminal:pty-1'],
			added: ['terminal:pty-1'],
			removed: [],
			move_back: false,
		});

		const map = useDetachedSurfaces.getState().surfaceToWindow;
		expect(map['terminal:pty-1']).toBe('detached-w2');
		expect(windowSurfaces('detached-w2').sort()).toEqual(['terminal:pty-1', 'terminal:pty-9']);
	});

	it('a move back drops the surface, arms its nudge and announces the return', () => {
		const seen: SurfacesReturned[] = [];
		const off = onSurfacesReturned((e) => seen.push(e));
		markSurfaceDetached('terminal:pty-1', 'detached-w2');
		markSurfaceDetached('terminal:pty-9', 'detached-w2');

		handleSurfacesChanged({
			label: 'detached-w2',
			surface_set: ['terminal:pty-9'],
			added: [],
			removed: ['terminal:pty-1'],
			move_back: true,
		});
		off();

		expect(isDetached('terminal:pty-1')).toBe(false);
		expect(isDetached('terminal:pty-9')).toBe(true);
		expect(hasPendingReclaimNudge('terminal:pty-1')).toBe(true);
		expect(seen).toEqual([{ label: 'detached-w2', surfaceIds: ['terminal:pty-1'], reason: 'move-back' }]);
	});

	it('a plain removal (a reclaim from the primary) announces nothing', () => {
		const seen: SurfacesReturned[] = [];
		const off = onSurfacesReturned((e) => seen.push(e));
		markSurfaceDetached('terminal:pty-1', 'detached-w2');
		handleSurfacesChanged({
			label: 'detached-w2',
			surface_set: [],
			added: [],
			removed: ['terminal:pty-1'],
			move_back: false,
		});
		off();
		expect(seen).toEqual([]);
	});

	it('a window closing with tabs in it announces every surface it held', async () => {
		const seen: SurfacesReturned[] = [];
		const off = onSurfacesReturned((e) => seen.push(e));
		markSurfaceDetached('terminal:pty-1', 'detached-w2');
		markSurfaceDetached('terminal:pty-9', 'detached-w2');
		markSurfaceDetached('viewer:/b.md', 'detached-viewer-1');
		mockListWindows.mockResolvedValue([descriptor('detached-viewer-1', ['viewer:/b.md'])]);

		await syncDetachedSurfaces();
		off();

		expect(seen).toHaveLength(1);
		expect(seen[0].label).toBe('detached-w2');
		expect(seen[0].reason).toBe('window-closed');
		expect(seen[0].surfaceIds.sort()).toEqual(['terminal:pty-1', 'terminal:pty-9']);
	});

	it('a re-sync keeps a provisional Pop out entry the registry doesn’t list yet', async () => {
		markSurfaceDetached('terminal:pty-1', PENDING_WINDOW_LABEL);
		mockListWindows.mockResolvedValue([]);

		await syncDetachedSurfaces();

		expect(useDetachedSurfaces.getState().surfaceToWindow['terminal:pty-1']).toBe(PENDING_WINDOW_LABEL);
		expect(hasPendingReclaimNudge('terminal:pty-1')).toBe(false);
	});

	it('clearPendingSurface drops only a provisional entry', () => {
		markSurfaceDetached('terminal:pty-1', PENDING_WINDOW_LABEL);
		markSurfaceDetached('terminal:pty-9', 'detached-w2');
		clearPendingSurface('terminal:pty-1');
		clearPendingSurface('terminal:pty-9');
		expect(isDetached('terminal:pty-1')).toBe(false);
		expect(isDetached('terminal:pty-9')).toBe(true);
	});
});
