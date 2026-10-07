// WP-71a — a pane's Pop out joins Window 2 (DEC-69d, G-SEATS §4.4, D-09
// pane ⋯ "Pop out to Window 2"): `TerminalView` and `ArtifactView` go through
// `popOutSurface`, so they join an open Window 2 and spawn one only when none
// is open, keeping their own surface id and window kind for the spawn. The
// surface is marked detached before the IPC; a failure un-marks it and says
// so. Written under DEC-50: not run here.

import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { SeatView } from '@/lib/tauri-cmd';

const m = vi.hoisted(() => ({
	// Gap audit rank 16: flips the page into a remote browser session.
	remote: false,
	spawnWindow: vi.fn(async (d: { label: string }) => d.label),
	// `null` = no Window 2 open (the caller spawns).
	windowJoinSurface: vi.fn(async (): Promise<string | null> => null),
	windowRemoveSurface: vi.fn(async () => [] as string[]),
	listWindows: vi.fn(async () => [] as unknown[]),
}));

vi.mock('@/lib/transport', async (orig) => ({
	...(await orig<typeof import('@/lib/transport')>()),
	listen: vi.fn(() => Promise.resolve(() => {})),
	isRemoteWebSession: () => m.remote,
}));
vi.mock('@/lib/iyke/client', () => ({
	iykeFetch: vi.fn(async () => ({ ok: false, json: async () => ({}) })),
}));
vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	spawnWindow: m.spawnWindow,
	windowJoinSurface: m.windowJoinSurface,
	windowRemoveSurface: m.windowRemoveSurface,
	listWindows: m.listWindows,
	seatsList: vi.fn(async () => []),
	chiList: vi.fn(async () => []),
	settingsGet: vi.fn(async () => null),
	settingsSet: vi.fn(async () => {}),
}));

// The live surfaces themselves are out of scope: only the Pop out path is.
vi.mock('@/terminal/single-terminal', () => ({
	SingleTerminal: () => <div data-testid="xterm" />,
}));
vi.mock('@/terminal/cost-hud', () => ({ CostHud: () => null }));
vi.mock('@/shell/wsl-health/wsl-health-banner', () => ({ WslHealthBanner: () => null }));
vi.mock('@/terminal/git-ledger', () => ({ GitLedger: () => null }));
vi.mock('@/terminal/permission-inbox', () => ({ PermissionInbox: () => null }));
vi.mock('@/terminal/tool-call-feed', () => ({ ToolCallFeed: () => null }));
vi.mock('@/terminal/transcript-replay', () => ({ TranscriptReplay: () => null }));
vi.mock('@/viewer/auto-router', () => ({ ViewerRouter: () => <div data-testid="viewer" /> }));
vi.mock('@/viewer/chrome/artifact-info-strip', () => ({ ArtifactInfoStrip: () => null }));
vi.mock('@/viewer/chrome/artifact-stopped-plate', () => ({ ArtifactStoppedPlate: () => null }));
vi.mock('@/viewer/chrome/use-artifact-disk-watch', () => ({
	useArtifactDiskWatch: () => ({ changed: false, reloadKey: 0, dismiss: () => {} }),
}));
vi.mock('@/viewer/chrome/use-viewer-server-health', () => ({
	useViewerServerHealth: () => ({ stopped: false, restart: () => {} }),
}));
vi.mock('@/viewer/history/version-history-panel', () => ({ VersionHistoryPanel: () => null }));

import { usePaneStore } from '@/lib/panes/pane-store';
import { seatsQueryKey } from '@/lib/queries/seats';
import { queryClient } from '@/lib/query-client';
import { useShellStore } from '@/lib/shell/shell-store';
import { syncDetachedSurfaces, useDetachedSurfaces } from '@/lib/window/detached-surfaces';
import { popOutSurface } from '@/lib/window/window-two';
import { PENDING_WINDOW_LABEL } from '@/lib/window/surfaces-topic';
import { useSeatNotice } from '@/shell/companion/seat-notice';
import { __resetSessionNumbersForTests } from '@/shell/companion/seat-sessions';
import { useTerminalStore } from '@/terminal/session-store';
import { ArtifactView } from './artifact-view';
import { TerminalView } from './terminal-view';

const PROJECT = 'royalti-co';
const TERM_SURFACE = 'terminal:pty-term-3';
const FILE = '/w/docs/readme.md';
const VIEWER_SURFACE = `viewer:${FILE}`;

function terminalTab(id: string) {
	return {
		id,
		title: id,
		spec: { cwd: '/w', cmd: ['bash'] },
		ptyId: `pty-${id}`,
		status: 'running' as const,
		exitCode: null,
		createdAt: 3,
		owner: { kind: 'sidepane' as const },
	};
}

function surfaceMap(): Record<string, string> {
	return useDetachedSurfaces.getState().surfaceToWindow;
}

function notice() {
	return useSeatNotice.getState().notice;
}

beforeEach(() => {
	queryClient.clear();
	__resetSessionNumbersForTests();
	m.spawnWindow.mockReset().mockImplementation(async (d: { label: string }) => d.label);
	m.windowJoinSurface.mockReset().mockResolvedValue(null);
	m.listWindows.mockReset().mockResolvedValue([]);
	// Round 52: reset every shared store this file touches. The project goes
	// first — its switch resets Companion state that nothing here sets.
	useShellStore.setState({
		activeProjectId: PROJECT,
		activeProject: { id: PROJECT, root_path: '/w', extra_roots: [] },
		projects: [],
	});
	useSeatNotice.setState({ notice: null });
	// A previous test's Pop out leaves its surface marked detached.
	useDetachedSurfaces.setState({ surfaceToWindow: {} });
	useTerminalStore.setState({ tabs: [terminalTab('term-3')] } as never);
	usePaneStore.setState({
		root: {
			type: 'leaf',
			id: 'L1',
			tabs: [{ kind: 'terminal', sessionId: 'term-3' }],
			activeTabIdx: 0,
		},
		focusedId: 'L1',
	});
});

afterEach(() => {
	cleanup();
	m.remote = false;
});

describe('TerminalView Pop out', () => {
	it('joins an open Window 2 and spawns nothing', async () => {
		m.windowJoinSurface.mockResolvedValue('detached-terminal-w2');
		render(<TerminalView sessionId="term-3" />);
		fireEvent.click(screen.getByRole('button', { name: 'Pop out terminal' }));
		await waitFor(() => expect(surfaceMap()[TERM_SURFACE]).toBe('detached-terminal-w2'));
		expect(m.windowJoinSurface).toHaveBeenCalledWith(TERM_SURFACE, PROJECT);
		expect(m.spawnWindow).not.toHaveBeenCalled();
		await waitFor(() =>
			expect(notice()?.message).toMatch(
				/^session \d+ moved to Window 2 — its address is unchanged$/
			)
		);
		// The pane swaps to its "popped out" placeholder, not a live duplicate.
		expect(screen.queryByTestId('xterm')).toBeNull();
	});

	it('names the seat in the toast when the terminal is a seat’s', async () => {
		m.windowJoinSurface.mockResolvedValue('detached-terminal-w2');
		const lead = {
			id: 'seat-lead',
			project_id: PROJECT,
			name: 'lead',
			session: { kind: 'terminal', terminal_id: 'term-3', external_id: null, cwd: '/w' },
		} as unknown as SeatView;
		queryClient.setQueryData(seatsQueryKey(PROJECT), [lead]);
		render(<TerminalView sessionId="term-3" />);
		fireEvent.click(screen.getByRole('button', { name: 'Pop out terminal' }));
		await waitFor(() =>
			expect(notice()?.message).toBe('lead moved to Window 2 — its address is unchanged')
		);
	});

	it('spawns its own single-surface terminal window only when no Window 2 is open', async () => {
		render(<TerminalView sessionId="term-3" />);
		fireEvent.click(screen.getByRole('button', { name: 'Pop out terminal' }));
		await waitFor(() => expect(m.spawnWindow).toHaveBeenCalledTimes(1));
		const d = m.spawnWindow.mock.calls[0][0] as unknown as Record<string, unknown>;
		expect(d).toMatchObject({
			kind: 'single-surface',
			surface_set: [TERM_SURFACE],
			project_id: null,
		});
		expect(d.label).toMatch(/^detached-terminal-/);
		await waitFor(() => expect(surfaceMap()[TERM_SURFACE]).toBe(d.label));
	});

	it('marks the surface before the IPC, and a failure un-marks it and says so', async () => {
		let fail: (e: Error) => void = () => {};
		m.windowJoinSurface.mockImplementation(
			() =>
				new Promise<string | null>((_, reject) => {
					fail = reject;
				})
		);
		render(<TerminalView sessionId="term-3" />);
		fireEvent.click(screen.getByRole('button', { name: 'Pop out terminal' }));
		expect(surfaceMap()[TERM_SURFACE]).toBe(PENDING_WINDOW_LABEL);
		fail(new Error('ipc down'));
		await waitFor(() => expect(notice()?.variant).toBe('error'));
		expect(notice()?.message).toMatch(/^Couldn’t pop out session \d+: ipc down$/);
		expect(TERM_SURFACE in surfaceMap()).toBe(false);
		expect(m.spawnWindow).not.toHaveBeenCalled();
		// Back to the live terminal.
		await waitFor(() => expect(screen.getByTestId('xterm')).toBeTruthy());
	});
});

describe('ArtifactView Pop out', () => {
	it('joins an open Window 2 and spawns nothing', async () => {
		m.windowJoinSurface.mockResolvedValue('detached-terminal-w2');
		render(<ArtifactView path={FILE} paneId="L1" />);
		fireEvent.click(screen.getByRole('button', { name: 'Pop out viewer' }));
		await waitFor(() => expect(surfaceMap()[VIEWER_SURFACE]).toBe('detached-terminal-w2'));
		expect(m.windowJoinSurface).toHaveBeenCalledWith(VIEWER_SURFACE, PROJECT);
		expect(m.spawnWindow).not.toHaveBeenCalled();
		await waitFor(() => expect(notice()?.message).toBe('readme.md moved to Window 2'));
	});

	it('spawns its own single-surface viewer window only when no Window 2 is open', async () => {
		render(<ArtifactView path={FILE} paneId="L1" />);
		fireEvent.click(screen.getByRole('button', { name: 'Pop out viewer' }));
		await waitFor(() => expect(m.spawnWindow).toHaveBeenCalledTimes(1));
		const d = m.spawnWindow.mock.calls[0][0] as unknown as Record<string, unknown>;
		expect(d).toMatchObject({
			kind: 'single-surface',
			surface_set: [VIEWER_SURFACE],
			project_id: null,
		});
		expect(d.label).toMatch(/^detached-viewer-/);
	});

	it('a failed spawn un-marks the surface and says so', async () => {
		m.spawnWindow.mockRejectedValue(new Error('no window'));
		render(<ArtifactView path={FILE} paneId="L1" />);
		fireEvent.click(screen.getByRole('button', { name: 'Pop out viewer' }));
		await waitFor(() => expect(notice()?.variant).toBe('error'));
		expect(notice()?.message).toBe('Couldn’t pop out readme.md: no window');
		expect(VIEWER_SURFACE in surfaceMap()).toBe(false);
		await waitFor(() => expect(screen.getByTestId('viewer')).toBeTruthy());
	});
});

// Gap audit rank 16 — a browser session has no second window, and the daemon
// serves neither window_join_surface nor window_list. The Pop out controls are
// hidden there and nothing reaches those commands.
describe('Pop out in a remote browser session (gap rank 16)', () => {
	it('TerminalView offers no Pop out button', () => {
		m.remote = true;
		render(<TerminalView sessionId="term-3" />);
		expect(screen.queryByRole('button', { name: 'Pop out terminal' })).toBeNull();
		expect(screen.getByTestId('xterm')).toBeTruthy();
	});

	it('ArtifactView offers no Pop out button', () => {
		m.remote = true;
		render(<ArtifactView path={FILE} paneId="L1" />);
		expect(screen.queryByRole('button', { name: 'Pop out viewer' })).toBeNull();
	});

	it('a stray popOutSurface call rejects without calling the window commands or marking the surface', async () => {
		m.remote = true;
		await expect(popOutSurface(TERM_SURFACE, { projectId: PROJECT })).rejects.toThrow(
			/Desktop app only/
		);
		expect(m.windowJoinSurface).not.toHaveBeenCalled();
		expect(m.spawnWindow).not.toHaveBeenCalled();
		expect(TERM_SURFACE in surfaceMap()).toBe(false);
	});

	it('the detached-surfaces refresh does not call window_list', async () => {
		m.remote = true;
		await syncDetachedSurfaces();
		expect(m.listWindows).not.toHaveBeenCalled();
	});

	it('the desktop refresh still calls window_list', async () => {
		await syncDetachedSurfaces();
		expect(m.listWindows).toHaveBeenCalledTimes(1);
	});
});
