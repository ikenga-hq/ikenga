// WP-09 T2 / T3 / T6 — status-bar segments hide at zero, the permissions
// segment carries the pending-approvals count (same query + 15 s poll the
// rail's approvals button used) and deep-links /outbox/approvals, and the
// bar is one keyboard tab stop with ←/→/Home/End roving.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, render as rtlRender, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactElement } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { DerivedPkgs, PkgRowV2 } from '@/lib/pkgs/use-derived';

const mocks = vi.hoisted(() => ({
	paActionsList: vi.fn(),
	pkgSidecarCall: vi.fn(),
	derived: vi.fn(),
	openCommandPalette: vi.fn(),
}));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	paActionsList: mocks.paActionsList,
	pkgSidecarCall: mocks.pkgSidecarCall,
}));
vi.mock('@/lib/pkgs/use-derived', async (orig) => ({
	...(await orig<typeof import('@/lib/pkgs/use-derived')>()),
	usePkgsDerived: () => mocks.derived(),
}));
vi.mock('@/lib/iyke/client', async (orig) => ({
	...(await orig<typeof import('@/lib/iyke/client')>()),
	iykeFetch: () => Promise.reject(new Error('no bridge in tests')),
}));
vi.mock('@/lib/transport', async (orig) => ({
	...(await orig<typeof import('@/lib/transport')>()),
	listen: () => Promise.resolve(() => {}),
}));
vi.mock('./command-palette', () => ({ openCommandPalette: mocks.openCommandPalette }));

import { usePaneStore } from '@/lib/panes/pane-store';
import { queryKeys } from '@/lib/query-keys';
import { useShellStore } from '@/lib/shell/shell-store';
import type { Project } from '@/lib/tauri-cmd';
import { APPROVALS_REFETCH_MS, APPROVALS_ROUTE, NGWA_LINKS, StatusBar } from './status-bar';

const PROJECT = {
	id: 'default',
	display_name: 'Default',
	root_path: null,
	icon: null,
	color: null,
	description: null,
	position: 0,
	is_default: true,
	created_at: 1,
	archived_at: null,
} as Project;

function rows(n: number): PkgRowV2[] {
	return Array.from({ length: n }, (_, i) => ({ id: `p${i}` }) as PkgRowV2);
}

function derived(counts: { installed?: number; updates?: number; violations?: number }) {
	return {
		rows: [],
		installed: rows(counts.installed ?? 0),
		registry: [],
		updates: rows(counts.updates ?? 0),
		trust: [],
		violations: rows(counts.violations ?? 0),
		builtin: [],
		engine: [],
		user: [],
		sidecarsRunning: 0,
		isLoading: false,
		error: null,
	} satisfies DerivedPkgs;
}

let client: QueryClient;
function render(ui: ReactElement) {
	client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return rtlRender(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
}

const navigateFocused = vi.fn();

beforeEach(() => {
	mocks.paActionsList.mockReset().mockResolvedValue([]);
	mocks.pkgSidecarCall.mockReset().mockResolvedValue({ ok: false, stdout: '' });
	mocks.derived.mockReset().mockReturnValue(derived({}));
	mocks.openCommandPalette.mockReset();
	navigateFocused.mockReset();
	usePaneStore.setState({ navigateFocused });
	useShellStore.setState({
		projects: [PROJECT],
		activeProjectId: PROJECT.id,
		defaultEngineId: null,
		companion: { activeTarget: { kind: 'new', engine_id: null } },
	});
});
afterEach(() => {
	cleanup();
	vi.useRealTimers();
});

function seg(id: string) {
	return screen.getByTestId('status-bar').querySelector(`[data-seg="${id}"]`);
}

describe('<StatusBar /> — zero hides (T2)', () => {
	it('renders no count segment when every count is zero', async () => {
		render(<StatusBar />);
		await waitFor(() => expect(mocks.paActionsList).toHaveBeenCalled());
		for (const id of [
			'branch',
			'modified',
			'ngwa',
			'ngwa-installed',
			'ngwa-updates',
			'ngwa-violations',
			'permissions',
			'runs',
			'cost',
			'engine',
		]) {
			expect(seg(id), `segment ${id} should be hidden at zero`).toBeNull();
		}
		// Always present: the read-only project item and the shortcuts button.
		expect(seg('project')?.textContent).toContain('Default');
		expect(seg('shortcuts')).not.toBeNull();
		// The Phase 5 notifications bell has a named, empty slot.
		const bell = screen.getByTestId('status-bar').querySelector('[data-slot="notifications-bell"]');
		expect(bell).not.toBeNull();
		expect(bell?.childNodes).toHaveLength(0);
	});

	it('hides each Ngwa segment independently and deep-links the rest', async () => {
		mocks.derived.mockReturnValue(derived({ installed: 22, updates: 0, violations: 2 }));
		const user = userEvent.setup();
		render(<StatusBar />);
		expect(seg('ngwa-installed')?.textContent).toBe('22 installed');
		expect(seg('ngwa-updates')).toBeNull();
		expect(seg('ngwa-violations')?.textContent).toBe('2 violations');
		await user.click(seg('ngwa-installed') as HTMLElement);
		expect(navigateFocused).toHaveBeenLastCalledWith(NGWA_LINKS.installed);
		await user.click(seg('ngwa-violations') as HTMLElement);
		expect(navigateFocused).toHaveBeenLastCalledWith(NGWA_LINKS.violations);
	});

	it('shows branch + modified from the git pkg snapshot, hiding modified at zero', async () => {
		useShellStore.setState({ projects: [{ ...PROJECT, root_path: '/r' }] });
		mocks.pkgSidecarCall.mockResolvedValue({
			ok: true,
			stdout: JSON.stringify({ result: { ok: true, snapshot: { branch: 'main', unstaged: 0 } } }),
		});
		render(<StatusBar />);
		await waitFor(() => expect(seg('branch')?.textContent).toContain('main'));
		expect(seg('modified')).toBeNull();
	});
});

describe('<StatusBar /> — permissions (T3)', () => {
	it('shows the pending-approvals count and deep-links /outbox/approvals', async () => {
		mocks.paActionsList.mockResolvedValue([{ id: 'a' }, { id: 'b' }, { id: 'c' }]);
		const user = userEvent.setup();
		render(<StatusBar />);
		await waitFor(() => expect(seg('permissions')).not.toBeNull());
		expect(seg('permissions')?.textContent).toContain('3 permissions pending');
		// Same query key the approvals route invalidates.
		expect(client.getQueryData(queryKeys.paActions.list(undefined))).toHaveLength(3);
		// Announced through the polite live region.
		expect(screen.getByRole('status').textContent).toContain('3 permissions pending');
		await user.click(seg('permissions') as HTMLElement);
		expect(navigateFocused).toHaveBeenCalledWith(APPROVALS_ROUTE);
		expect(APPROVALS_ROUTE).toBe('/outbox/approvals');
	});

	it('polls every 15 s, like the rail button it replaces', async () => {
		expect(APPROVALS_REFETCH_MS).toBe(15_000);
		vi.useFakeTimers({ shouldAdvanceTime: true });
		mocks.paActionsList.mockResolvedValue([{ id: 'a' }]);
		render(<StatusBar />);
		await waitFor(() => expect(mocks.paActionsList).toHaveBeenCalledTimes(1));
		await act(async () => {
			await vi.advanceTimersByTimeAsync(APPROVALS_REFETCH_MS + 50);
		});
		await waitFor(() => expect(mocks.paActionsList).toHaveBeenCalledTimes(2));
		expect(seg('permissions')?.textContent).toContain('1 permission pending');
	});
});

describe('<StatusBar /> — keyboard (T6)', () => {
	it('is one tab stop with arrow / Home / End roving over every button', async () => {
		mocks.derived.mockReturnValue(derived({ installed: 4, updates: 1 }));
		mocks.paActionsList.mockResolvedValue([{ id: 'a' }]);
		const user = userEvent.setup();
		render(<StatusBar />);
		await waitFor(() => expect(seg('permissions')).not.toBeNull());

		const bar = screen.getByRole('toolbar', { name: 'Status bar' });
		const buttons = Array.from(bar.querySelectorAll('button'));
		expect(buttons.map((b) => b.dataset.seg)).toEqual([
			'ngwa-installed',
			'ngwa-updates',
			'permissions',
			'shortcuts',
		]);
		expect(buttons.filter((b) => b.tabIndex === 0)).toHaveLength(1);

		await user.tab();
		expect(document.activeElement).toBe(seg('ngwa-installed'));
		await user.keyboard('{ArrowRight}');
		expect(document.activeElement).toBe(seg('ngwa-updates'));
		await user.keyboard('{End}');
		expect(document.activeElement).toBe(seg('shortcuts'));
		await user.keyboard('{ArrowRight}');
		expect(document.activeElement).toBe(seg('ngwa-installed'));
		await user.keyboard('{ArrowLeft}');
		expect(document.activeElement).toBe(seg('shortcuts'));
		// The roving stop follows focus.
		expect((seg('shortcuts') as HTMLButtonElement).tabIndex).toBe(0);
		await user.keyboard('{Enter}');
		expect(mocks.openCommandPalette).toHaveBeenCalledWith('shortcuts');
		await user.keyboard('{Home}');
		expect(document.activeElement).toBe(seg('ngwa-installed'));
	});

	it('shows the engine for the next dispatch as read-only (no tab stop)', () => {
		useShellStore.setState({ defaultEngineId: 'claude-code' });
		render(<StatusBar />);
		const engine = seg('engine') as HTMLElement;
		expect(engine.textContent).toContain('claude-code');
		expect(engine.tagName).toBe('SPAN');
	});
});
