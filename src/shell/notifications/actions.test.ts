// WP-40b — `notificationActionButtons` is the single place popover rows and
// toast pills decide what "Allow once", "Review", "Open log" etc. actually
// do. Covers every `NotificationAction.kind` the producers emit
// (`src-tauri/src/notifications/producers.rs`) plus the no-action /
// unknown-kind fallbacks.

import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { NotificationRow } from '@/lib/tauri-cmd';

const mocks = vi.hoisted(() => ({
	navigateFocused: vi.fn(),
	addTab: vi.fn(),
	iykeFetch: vi.fn(() => Promise.resolve(new Response(null, { status: 204 }))),
}));

vi.mock('@/lib/panes/pane-store', () => ({
	usePaneStore: {
		getState: () => ({
			focusedId: 'pane-1',
			navigateFocused: mocks.navigateFocused,
			addTab: mocks.addTab,
		}),
	},
}));

vi.mock('@/lib/iyke/client', () => ({
	iykeFetch: mocks.iykeFetch,
}));

import { notificationActionButtons } from './actions';

function row(overrides: Partial<NotificationRow>): NotificationRow {
	return {
		id: 1,
		kind: 'permission',
		title: 't',
		body: null,
		action: null,
		source: 'test',
		dedupeKey: null,
		count: 1,
		createdAt: 0,
		updatedAt: 0,
		readAt: null,
		...overrides,
	};
}

beforeEach(() => {
	mocks.navigateFocused.mockClear();
	mocks.addTab.mockClear();
	mocks.iykeFetch.mockClear();
});

describe('notificationActionButtons', () => {
	it('permission.decide: Allow once posts approved, Deny posts denied', () => {
		const buttons = notificationActionButtons(
			row({
				kind: 'permission',
				action: { kind: 'permission.decide', via: 'hooks', requestId: 'req-1', terminalId: 't-1' },
			})
		);
		expect(buttons.map((b) => b.label)).toEqual(['Allow once', 'Deny']);
		expect(buttons[0]?.variant).toBe('primary');
		expect(buttons[1]?.variant).toBe('ghost');

		buttons[0]?.run();
		expect(mocks.iykeFetch).toHaveBeenCalledWith(
			'/iyke/hooks/decision',
			expect.objectContaining({
				method: 'POST',
				body: JSON.stringify({ requestId: 'req-1', decision: 'approved' }),
			})
		);

		buttons[1]?.run();
		expect(mocks.iykeFetch).toHaveBeenLastCalledWith(
			'/iyke/hooks/decision',
			expect.objectContaining({ body: JSON.stringify({ requestId: 'req-1', decision: 'denied' }) })
		);
	});

	it('open.terminal opens a terminal pane keyed by terminalId, falling back to sessionId', () => {
		const withTerminal = notificationActionButtons(
			row({ action: { kind: 'open.terminal', terminalId: 'term-1', sessionId: null } })
		);
		withTerminal[0]?.run();
		expect(mocks.addTab).toHaveBeenCalledWith('pane-1', { kind: 'terminal', sessionId: 'term-1' });

		mocks.addTab.mockClear();
		const sessionOnly = notificationActionButtons(
			row({ action: { kind: 'open.terminal', terminalId: null, sessionId: 'sess-1' } })
		);
		sessionOnly[0]?.run();
		expect(mocks.addTab).toHaveBeenCalledWith('pane-1', { kind: 'terminal', sessionId: 'sess-1' });
	});

	it('open.terminal with neither id renders no button', () => {
		const buttons = notificationActionButtons(
			row({ action: { kind: 'open.terminal', terminalId: null, sessionId: null } })
		);
		expect(buttons).toEqual([]);
	});

	it('open.thread opens a terminal pane keyed by threadId', () => {
		const buttons = notificationActionButtons(
			row({ action: { kind: 'open.thread', threadId: 'thread-1', requestId: 'req-2' } })
		);
		expect(buttons.map((b) => b.label)).toEqual(['Open thread']);
		buttons[0]?.run();
		expect(mocks.addTab).toHaveBeenCalledWith('pane-1', { kind: 'terminal', sessionId: 'thread-1' });
	});

	it('open.chi_run labels "Open log" on failure, "Open artifact" on success, both route to /automations', () => {
		const failed = notificationActionButtons(
			row({ kind: 'run_failed', action: { kind: 'open.chi_run', runId: 'r1', status: 'failed' } })
		);
		expect(failed.map((b) => b.label)).toEqual(['Open log']);
		failed[0]?.run();
		expect(mocks.navigateFocused).toHaveBeenLastCalledWith('/automations?view=runs');

		const done = notificationActionButtons(
			row({ kind: 'run_finished', action: { kind: 'open.chi_run', runId: 'r2', status: 'done' } })
		);
		expect(done.map((b) => b.label)).toEqual(['Open artifact']);
	});

	it('open.release_notes routes to /settings/about', () => {
		const buttons = notificationActionButtons(
			row({ kind: 'update', action: { kind: 'open.release_notes', source: 'shell', version: '0.9.1' } })
		);
		buttons[0]?.run();
		expect(mocks.navigateFocused).toHaveBeenCalledWith('/settings/about');
	});

	it('open.pkg_updates routes to the Ngwa updates filter', () => {
		const buttons = notificationActionButtons(
			row({
				kind: 'update',
				action: { kind: 'open.pkg_updates', pkgId: 'com.ikenga.iyke', version: '1.0.0' },
			})
		);
		buttons[0]?.run();
		expect(mocks.navigateFocused).toHaveBeenCalledWith('/packages?filter=updates');
	});

	it('open.violations routes to the Ngwa review filter as the primary action', () => {
		const buttons = notificationActionButtons(
			row({ kind: 'violation', action: { kind: 'open.violations', pkgId: 'com.ikenga.pkg-browser' } })
		);
		expect(buttons[0]?.variant).toBe('primary');
		buttons[0]?.run();
		expect(mocks.navigateFocused).toHaveBeenCalledWith('/packages?filter=review');
	});

	it('invite with no action yet (no D-05 producer) still offers Open People', () => {
		const buttons = notificationActionButtons(row({ kind: 'invite', action: null }));
		expect(buttons.map((b) => b.label)).toEqual(['Open People']);
		buttons[0]?.run();
		expect(mocks.navigateFocused).toHaveBeenCalledWith('/settings/people');
	});

	it('no action and a non-invite kind renders no buttons', () => {
		expect(notificationActionButtons(row({ kind: 'run_finished', action: null }))).toEqual([]);
	});

	it('an unrecognized action kind renders no buttons for a non-invite row', () => {
		const buttons = notificationActionButtons(
			row({ kind: 'update', action: { kind: 'some.future.action', foo: 'bar' } })
		);
		expect(buttons).toEqual([]);
	});
});
