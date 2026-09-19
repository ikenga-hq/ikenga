// Companion component tests — C2 (collapsed strip), C3 (permission card UI
// + A/D keys + Undo), C4 (tab drag between the Companion and the pane tree),
// C5 (no session → empty states with exactly one action), C7 (⌘2 /
// `ikenga:companion-focus` focuses the dispatch input), plus the dispatch
// bar's Enter / ⇧Enter / ⌥Enter routing.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const handlers: Record<string, (e: { payload: unknown }) => void> = {};
vi.mock('@/lib/transport', async (orig) => ({
	...(await orig<typeof import('@/lib/transport')>()),
	listen: vi.fn((channel: string, handler: (e: { payload: unknown }) => void) => {
		handlers[channel] = handler;
		return Promise.resolve(() => {});
	}),
	isNotificationPermissionGranted: vi.fn(async () => true),
	requestNotificationPermission: vi.fn(async () => 'granted'),
	sendNotification: vi.fn(),
}));

const iykeFetch = vi.fn(async () => ({ ok: false, json: async () => ({}) }));
vi.mock('@/lib/iyke/client', () => ({ iykeFetch: (...a: unknown[]) => iykeFetch(...(a as [])) }));

const ptyWrite = vi.fn(async () => {});
const chiRun = vi.fn(async () => ({ run_id: 'run-1', status: 'queued' }));
vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	ptyWrite: (...a: unknown[]) => ptyWrite(...(a as [])),
	chiRun: (...a: unknown[]) => chiRun(...(a as [])),
	chiResume: vi.fn(async () => ({ run_id: 'x', status: 'running' })),
	chiList: vi.fn(async () => []),
	detectAgents: vi.fn(async () => []),
	ptyTerminalList: vi.fn(async () => []),
	settingsGet: vi.fn(async () => null),
	settingsSet: vi.fn(async () => {}),
}));

// pane-views pulls in every pane body (xterm, editors, emoji data) — the
// Companion only needs its tab labels.
vi.mock('@/shell/panes/pane-views', () => ({
	viewLabel: (v: { kind: string; sessionId?: string; path?: string }) =>
		v.kind === 'terminal' ? `terminal ${v.sessionId}` : (v.path ?? v.kind),
}));

import { useDragState } from '@/lib/panes/drag-state';
import { usePaneStore } from '@/lib/panes/pane-store';
import { beginPointerDrag } from '@/lib/panes/pointer-drag';
import { useShellStore } from '@/lib/shell/shell-store';
import { PaneDropZones } from '@/shell/panes/drop-zones';
import { CostHud } from '@/terminal/cost-hud';
import { ToolCallFeed } from '@/terminal/tool-call-feed';
import { COMPANION_FOCUS_EVENT, Companion } from './companion';
import {
	PERMISSION_UNDO_MS,
	__resetCompanionTimersForTests,
	useCompanionStore,
} from './companion-store';

function wrap(ui: ReactNode) {
	const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return render(<QueryClientProvider client={qc}>{ui}</QueryClientProvider>);
}

/** jsdom has no PointerEvent; a MouseEvent carrying a pointerId is enough. */
function pointer(type: string, x: number, y: number): Event {
	const ev = new MouseEvent(type, {
		clientX: x,
		clientY: y,
		bubbles: true,
		cancelable: true,
		button: 0,
	});
	Object.defineProperty(ev, 'pointerId', { value: 1 });
	return ev;
}

let stack: Element[] = [];

beforeEach(() => {
	__resetCompanionTimersForTests();
	stack = [];
	document.elementsFromPoint = vi.fn(() => stack);
	useCompanionStore.setState({
		state: 'collapsed',
		tabs: [],
		activeIdx: 0,
		width: 372,
		panelScopeSessionId: null,
		draft: '',
		focusPending: false,
		pickerPending: false,
		permissions: [],
		quietSince: null,
	});
	useShellStore.setState({
		companion: { activeTarget: { kind: 'new', engine_id: null } },
		defaultEngineId: 'claude-code',
		// Mark the Chi gloss as seen so LoreTerm renders plain text.
		onboarding: { ...useShellStore.getState().onboarding, loreGlossSeen: ['chi'] },
	});
	usePaneStore.setState({
		root: {
			type: 'leaf',
			id: 'L1',
			tabs: [{ kind: 'terminal', sessionId: 'term-a' }],
			activeTabIdx: 0,
		},
		focusedId: 'L1',
	});
	ptyWrite.mockClear();
	chiRun.mockClear();
	iykeFetch.mockClear();
});

afterEach(async () => {
	cleanup();
	window.dispatchEvent(new Event('blur')); // release any pointer-drag latch
	useDragState.getState().end();
	__resetCompanionTimersForTests();
	vi.useRealTimers();
	// A completed drag swallows the next click until a 0 ms timer clears it
	// (`suppressNextClick`); let that timer run before the next test clicks.
	await new Promise((r) => setTimeout(r, 0));
});

describe('C2 — collapsed strip (§5.1)', () => {
	it('is one button whose accessible name carries the pending count and the key', () => {
		useCompanionStore
			.getState()
			.receivePermission({ id: 'r1', kind: 'permission', toolName: 'Bash' });
		useCompanionStore.setState({ state: 'collapsed' });
		wrap(<Companion />);
		const strip = screen.getByRole('complementary', { name: 'Chi companion' });
		const buttons = within(strip).getAllByRole('button');
		expect(buttons).toHaveLength(1);
		expect(buttons[0].getAttribute('aria-expanded')).toBe('false');
		expect(buttons[0].getAttribute('aria-label')).toMatch(
			/^Chi companion, collapsed, 1 permission pending\. Expand \((⌘J|Ctrl\+J)\)\.$/
		);
		expect(strip.querySelector('[data-attention="permission"]')).not.toBeNull();
		expect(strip.textContent).toContain('Chi · 1 pending');
	});

	it('with nothing pending the strip shows no attention glyph and a plain label', () => {
		wrap(<Companion />);
		const strip = screen.getByRole('complementary', { name: 'Chi companion' });
		expect(strip.querySelector('[data-attention]')).toBeNull();
		expect(strip.textContent).toBe('Chi');
	});

	it('clicking the strip expands the Companion and focuses the dispatch input', async () => {
		wrap(<Companion />);
		fireEvent.click(screen.getByRole('button', { name: /^Chi companion, collapsed/ }));
		const input = await screen.findByRole('textbox', { name: 'Dispatch an instruction' });
		await waitFor(() => expect(document.activeElement).toBe(input));
	});

	it('an incoming permission request auto-expands the Companion (hooks bus)', async () => {
		wrap(<Companion />);
		await waitFor(() => expect(handlers['hooks://event']).toBeDefined());
		act(() => {
			handlers['hooks://event']({
				payload: { hook_event_name: 'PermissionRequest', request_id: 'r7', tool_name: 'Write' },
			});
		});
		expect(useCompanionStore.getState().state).toBe('expanded');
		expect(await screen.findByRole('group', { name: 'Permission request: Write' })).toBeTruthy();
		// Focus is never stolen.
		expect(document.activeElement).toBe(document.body);
	});
});

describe('C7 — ⌘2 / ikenga:companion-focus', () => {
	it('expands the Companion and focuses the dispatch input', async () => {
		wrap(<Companion />);
		act(() => {
			window.dispatchEvent(new CustomEvent(COMPANION_FOCUS_EVENT));
		});
		const input = await screen.findByRole('textbox', { name: 'Dispatch an instruction' });
		await waitFor(() => expect(document.activeElement).toBe(input));
		expect(useCompanionStore.getState().state).toBe('expanded');
	});

	it('focuses the input when the Companion is already expanded too', async () => {
		useCompanionStore.setState({ state: 'expanded' });
		wrap(<Companion />);
		const input = screen.getByRole('textbox', { name: 'Dispatch an instruction' });
		expect(document.activeElement).not.toBe(input);
		act(() => {
			window.dispatchEvent(new CustomEvent(COMPANION_FOCUS_EVENT));
		});
		await waitFor(() => expect(document.activeElement).toBe(input));
	});
});

describe('dispatch bar', () => {
	it('Enter sends to the live terminal target via ptyWrite and clears the input', async () => {
		const { useTerminalStore } = await import('@/terminal/session-store');
		useTerminalStore.setState({
			tabs: [
				{
					id: 'term-a',
					title: 'bash',
					spec: { cwd: '/', cmd: ['bash'] },
					ptyId: 'pty-a',
					status: 'running',
					exitCode: null,
					createdAt: 0,
					owner: { kind: 'sidepane' },
				},
			],
		});
		useShellStore.setState({
			companion: { activeTarget: { kind: 'session', session_id: 'term-a' } },
		});
		useCompanionStore.setState({ state: 'expanded' });
		wrap(<Companion />);
		const input = screen.getByRole('textbox', { name: 'Dispatch an instruction' });
		fireEvent.change(input, { target: { value: 'ls -la' } });
		fireEvent.keyDown(input, { key: 'Enter' });
		await waitFor(() => expect(ptyWrite).toHaveBeenCalledTimes(1));
		const [ptyId, data] = ptyWrite.mock.calls[0] as unknown as [string, string];
		expect(ptyId).toBe('pty-a');
		expect(data.endsWith('ls -la\r')).toBe(true);
		expect(data.startsWith('# ikenga · ')).toBe(true);
		await waitFor(() => expect((input as HTMLInputElement).value).toBe(''));
		// ↑ recalls it.
		fireEvent.keyDown(input, { key: 'ArrowUp' });
		expect((input as HTMLInputElement).value).toBe('ls -la');
		useTerminalStore.setState({ tabs: [] });
	});

	it('⇧Enter starts a new run and ⌥Enter a persistent one', async () => {
		useCompanionStore.setState({ state: 'expanded' });
		wrap(<Companion />);
		const input = screen.getByRole('textbox', { name: 'Dispatch an instruction' });
		fireEvent.change(input, { target: { value: 'one' } });
		fireEvent.keyDown(input, { key: 'Enter', shiftKey: true });
		await waitFor(() =>
			expect(chiRun).toHaveBeenCalledWith(
				expect.objectContaining({ persistent: false, engineId: 'claude-code' })
			)
		);
		await waitFor(() => expect((input as HTMLInputElement).value).toBe(''));
		fireEvent.change(input, { target: { value: 'two' } });
		fireEvent.keyDown(input, { key: 'Enter', altKey: true });
		await waitFor(() =>
			expect(chiRun).toHaveBeenLastCalledWith(expect.objectContaining({ persistent: true }))
		);
	});

	it('with no engine, ⌘2 lands on the target chip (the input is disabled)', async () => {
		useShellStore.setState({ defaultEngineId: null });
		wrap(<Companion />);
		act(() => {
			window.dispatchEvent(new CustomEvent(COMPANION_FOCUS_EVENT));
		});
		const chip = await screen.findByRole('button', { name: /^Dispatch target:/ });
		await waitFor(() => expect(document.activeElement).toBe(chip));
	});

	it('is disabled with the Ngwa reason when no engine resolves', () => {
		useShellStore.setState({ defaultEngineId: null });
		useCompanionStore.setState({ state: 'expanded' });
		wrap(<Companion />);
		const input = screen.getByRole('textbox', { name: 'Dispatch an instruction' });
		expect((input as HTMLInputElement).disabled).toBe(true);
		expect(input.getAttribute('title')).toBe('No engine installed — open Ngwa → Store');
	});

	it('"Hand to Chi" pre-fills and focuses the input without sending', async () => {
		const { handToChi } = await import('./companion-store');
		wrap(<Companion />);
		act(() => handToChi('Look at src/app.ts'));
		const input = await screen.findByRole('textbox', { name: 'Dispatch an instruction' });
		expect((input as HTMLInputElement).value).toBe('Look at src/app.ts');
		await waitFor(() => expect(document.activeElement).toBe(input));
		expect(chiRun).not.toHaveBeenCalled();
		expect(ptyWrite).not.toHaveBeenCalled();
	});
});

describe('C3 — permission card (§5.6)', () => {
	it('A on the focused card allows; Undo inside 5 s restores it; nothing is posted', async () => {
		vi.useFakeTimers({ shouldAdvanceTime: true });
		useCompanionStore
			.getState()
			.receivePermission({ id: 'r1', kind: 'permission', toolName: 'Bash' });
		wrap(<Companion />);
		const card = screen.getByRole('group', { name: 'Permission request: Bash' });
		act(() => card.focus());
		fireEvent.keyDown(card, { key: 'a' });
		expect(card.getAttribute('data-status')).toBe('undoable');
		expect(within(card).getByRole('status').textContent).toContain('Allowed once');

		fireEvent.click(within(card).getByRole('button', { name: 'Undo' }));
		expect(card.getAttribute('data-status')).toBe('pending');
		await act(async () => {
			await vi.advanceTimersByTimeAsync(PERMISSION_UNDO_MS * 2);
		});
		expect(iykeFetch).not.toHaveBeenCalledWith('/iyke/hooks/decision', expect.anything());
	});

	it('D denies; after 5 s the decision is posted and the card is resolved', async () => {
		vi.useFakeTimers({ shouldAdvanceTime: true });
		useCompanionStore
			.getState()
			.receivePermission({ id: 'r2', kind: 'permission', toolName: 'Edit' });
		wrap(<Companion />);
		const card = screen.getByRole('group', { name: 'Permission request: Edit' });
		act(() => card.focus());
		fireEvent.keyDown(card, { key: 'd' });
		await act(async () => {
			await vi.advanceTimersByTimeAsync(PERMISSION_UNDO_MS);
		});
		expect(iykeFetch).toHaveBeenCalledWith(
			'/iyke/hooks/decision',
			expect.objectContaining({ body: JSON.stringify({ requestId: 'r2', decision: 'denied' }) })
		);
		expect(card.getAttribute('data-status')).toBe('resolved');
		expect(card.textContent).toContain('Denied');
	});

	it('the pointer path: Allow once button', () => {
		useCompanionStore
			.getState()
			.receivePermission({ id: 'r3', kind: 'permission', toolName: 'Read' });
		wrap(<Companion />);
		const card = screen.getByRole('group', { name: 'Permission request: Read' });
		fireEvent.click(within(card).getByRole('button', { name: 'Allow once' }));
		expect(card.getAttribute('data-status')).toBe('undoable');
		expect(within(card).getByRole('button', { name: 'Undo' })).toBeTruthy();
	});
});

describe('C5 — no session selected', () => {
	it.each([
		['CostHud', () => <CostHud sessionId={null} />],
		['ToolCallFeed', () => <ToolCallFeed />],
	])('%s renders its empty state with exactly one action, no throw', (_name, make) => {
		const { container } = wrap(make());
		const buttons = within(container).getAllByRole('button');
		expect(buttons).toHaveLength(1);
		expect(buttons[0].textContent).toBe('Choose a session');
		expect(container.querySelector('input, textarea, select')).toBeNull();
	});

	it('inside the Companion, both scoped panels show their empty action', () => {
		useCompanionStore.setState({ state: 'expanded' });
		wrap(<Companion />);
		expect(screen.getAllByRole('button', { name: 'Choose a session' })).toHaveLength(2);
	});

	it('the empty action opens the target picker', async () => {
		useCompanionStore.setState({ state: 'expanded' });
		wrap(<Companion />);
		fireEvent.click(screen.getAllByRole('button', { name: 'Choose a session' })[0]);
		expect(await screen.findByRole('menu', { name: 'Dispatch targets' })).toBeTruthy();
	});
});

describe('C12 — one input, no PermissionInbox (ADR-021 / spec §5.4, §5.6)', () => {
	it('the expanded Companion, with live sessions and a pending request, has exactly one text input', async () => {
		const { useTerminalStore } = await import('@/terminal/session-store');
		useTerminalStore.setState({
			tabs: [
				{
					id: 'term-a',
					title: 'claude',
					spec: { cwd: '/work', cmd: ['claude'] },
					ptyId: 'pty-a',
					status: 'running',
					exitCode: null,
					createdAt: 0,
					owner: { kind: 'sidepane' },
				},
			],
		});
		useCompanionStore
			.getState()
			.receivePermission({ id: 'r9', kind: 'permission', toolName: 'Bash' });
		useCompanionStore.setState({ state: 'expanded' });
		try {
			const { container } = wrap(<Companion />);
			// MissionControl rendered its session grid (not its empty state)…
			expect(screen.getByText(/Mission Control \(1 sessions active\)/)).toBeTruthy();
			// …without its own dispatcher: the dispatch bar is the one input.
			expect(screen.getAllByRole('textbox')).toHaveLength(1);
			expect(container.querySelectorAll('input, textarea, select, [contenteditable]')).toHaveLength(
				1
			);
			expect(screen.queryByPlaceholderText(/Dispatch prompt across sessions/)).toBeNull();
			// …and without its sample model / cost figures.
			expect(screen.queryByText('Claude 3.5 Sonnet')).toBeNull();
			expect(screen.queryByText('$0.024')).toBeNull();
			// Permissions: the §5.6 card, never the scoped PermissionInbox HUD.
			expect(screen.getByRole('group', { name: 'Permission request: Bash' })).toBeTruthy();
			expect(screen.queryByText(/Permission Inbox/)).toBeNull();
		} finally {
			useTerminalStore.setState({ tabs: [] });
		}
	});
});

describe('C4 — tab drag between the Companion and the pane tree', () => {
	it('drags a session tab out of the Companion into a pane', () => {
		useCompanionStore.setState({
			state: 'expanded',
			tabs: [{ kind: 'terminal', sessionId: 'term-z' }],
		});
		usePaneStore.setState({
			root: { type: 'leaf', id: 'L1', tabs: [{ kind: 'route', path: '/' }], activeTabIdx: 0 },
			focusedId: 'L1',
		});
		wrap(
			<div>
				<div style={{ position: 'relative' }}>
					<PaneDropZones paneId="L1" />
				</div>
				<Companion />
			</div>
		);
		const tab = screen.getByRole('tab');
		const zone = screen.getByTestId('drop-zone-L1');
		// jsdom lays nothing out; give the pane a box so (40, 40) is its centre
		// zone (move-as-tab), not an edge (split).
		zone.getBoundingClientRect = () =>
			({ left: 0, top: 0, width: 80, height: 80, right: 80, bottom: 80, x: 0, y: 0 }) as DOMRect;

		act(() => {
			tab.dispatchEvent(pointer('pointerdown', 10, 10));
		});
		stack = [zone];
		act(() => {
			window.dispatchEvent(pointer('pointermove', 40, 40));
		});
		expect(useDragState.getState()).toMatchObject({ active: true, source: 'dock', srcTabIdx: 0 });
		act(() => {
			window.dispatchEvent(pointer('pointerup', 40, 40));
		});

		const leaf = usePaneStore.getState().root;
		expect(leaf.type === 'leaf' && leaf.tabs).toEqual([
			{ kind: 'route', path: '/' },
			{ kind: 'terminal', sessionId: 'term-z' },
		]);
		expect(useCompanionStore.getState().tabs).toEqual([]);
		expect(useDragState.getState().active).toBe(false);
	});

	it('drops a pane terminal tab onto the collapsed strip: a session tab, pane keeps it', () => {
		wrap(<Companion />);
		const strip = screen.getByRole('complementary', { name: 'Chi companion' });
		const source = document.createElement('div');
		document.body.appendChild(source);
		beginPointerDrag(
			{ button: 0, clientX: 5, clientY: 5, pointerId: 1, currentTarget: source },
			{
				label: 'bash',
				onStart: () => useDragState.getState().startPane('L1', 0),
				onEnd: () => useDragState.getState().end(),
			}
		);
		stack = [strip];
		act(() => {
			window.dispatchEvent(pointer('pointermove', 30, 30));
			window.dispatchEvent(pointer('pointerup', 30, 30));
		});
		const s = useCompanionStore.getState();
		expect(s.tabs).toEqual([{ kind: 'terminal', sessionId: 'term-a' }]);
		expect(s.state).toBe('expanded');
		expect(s.panelScopeSessionId).toBe('term-a');
		const leaf = usePaneStore.getState().root;
		expect(leaf.type === 'leaf' && leaf.tabs).toEqual([{ kind: 'terminal', sessionId: 'term-a' }]);
	});

	it('does not accept a non-terminal pane tab (only sessions live here)', () => {
		usePaneStore.setState({
			root: { type: 'leaf', id: 'L1', tabs: [{ kind: 'route', path: '/files' }], activeTabIdx: 0 },
			focusedId: 'L1',
		});
		wrap(<Companion />);
		const strip = screen.getByRole('complementary', { name: 'Chi companion' });
		const source = document.createElement('div');
		document.body.appendChild(source);
		beginPointerDrag(
			{ button: 0, clientX: 5, clientY: 5, pointerId: 1, currentTarget: source },
			{
				label: 'files',
				onStart: () => useDragState.getState().startPane('L1', 0),
				onEnd: () => useDragState.getState().end(),
			}
		);
		stack = [strip];
		act(() => {
			window.dispatchEvent(pointer('pointermove', 30, 30));
			window.dispatchEvent(pointer('pointerup', 30, 30));
		});
		expect(useCompanionStore.getState().tabs).toEqual([]);
	});

	it('"Move to pane" in the tab menu is the single-pointer alternative (WCAG 2.5.7)', () => {
		useCompanionStore.setState({
			state: 'expanded',
			tabs: [{ kind: 'terminal', sessionId: 'term-q' }],
		});
		usePaneStore.setState({
			root: { type: 'leaf', id: 'L1', tabs: [{ kind: 'route', path: '/' }], activeTabIdx: 0 },
			focusedId: 'L1',
		});
		wrap(<Companion />);
		fireEvent.contextMenu(screen.getByRole('tab'));
		fireEvent.click(screen.getByRole('menuitem', { name: 'Move to pane' }));
		const leaf = usePaneStore.getState().root;
		expect(leaf.type === 'leaf' && leaf.tabs.at(-1)).toEqual({
			kind: 'terminal',
			sessionId: 'term-q',
		});
		expect(useCompanionStore.getState().tabs).toEqual([]);
	});
});
