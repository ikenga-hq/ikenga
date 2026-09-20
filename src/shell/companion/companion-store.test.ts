// Companion store — C2 (auto-expand triggers, spec §5.2) and C3 (permission
// card resolve + 5 s undo, spec §5.6), plus the session-tab contract
// (selecting a tab sets panel scope AND G-STATE's activeTarget).

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const iykeFetch = vi.fn(async () => ({ ok: true, json: async () => ({}) }));
vi.mock('@/lib/iyke/client', () => ({ iykeFetch: (...a: unknown[]) => iykeFetch(...(a as [])) }));

const fsRead = vi.fn();
const fsWriteText = vi.fn(async () => {});
vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	fsRead: (...a: unknown[]) => fsRead(...a),
	fsWriteText: (...a: unknown[]) => fsWriteText(...(a as [])),
}));

import { useShellStore } from '@/lib/shell/shell-store';
import {
	PERMISSION_QUIET_MS,
	PERMISSION_UNDO_MS,
	__resetCompanionTimersForTests,
	focusCompanion,
	handToChi,
	useCompanionStore,
} from './companion-store';

function reset() {
	__resetCompanionTimersForTests();
	useCompanionStore.setState({
		state: 'collapsed',
		tabs: [],
		activeIdx: 0,
		panelScopeSessionId: null,
		draft: '',
		focusPending: false,
		pickerPending: false,
		permissions: [],
		quietSince: null,
	});
	useShellStore.setState({ companion: { activeTarget: { kind: 'new', engine_id: null } } });
}

const req = (id: string) => ({
	id,
	kind: 'permission' as const,
	toolName: 'Bash',
	prompt: 'run ls',
});

beforeEach(() => {
	vi.useFakeTimers();
	vi.setSystemTime(new Date('2026-09-19T10:00:00Z'));
	reset();
	iykeFetch.mockClear();
	fsRead.mockReset();
	fsWriteText.mockClear();
});

afterEach(() => {
	__resetCompanionTimersForTests();
	vi.useRealTimers();
});

describe('C2 — auto-expand triggers (§5.2)', () => {
	it('⌘2 / focusCompanion expands and requests dispatch-input focus', () => {
		focusCompanion();
		const s = useCompanionStore.getState();
		expect(s.state).toBe('expanded');
		expect(s.focusPending).toBe(true);
	});

	it('⌘J toggles; expanding requests focus, collapsing does not', () => {
		const { cycleState } = useCompanionStore.getState();
		cycleState();
		expect(useCompanionStore.getState().state).toBe('expanded');
		expect(useCompanionStore.getState().focusPending).toBe(true);
		useCompanionStore.getState().consumeFocus();
		cycleState();
		expect(useCompanionStore.getState().state).toBe('collapsed');
		expect(useCompanionStore.getState().focusPending).toBe(false);
	});

	it('⌘J never cycles into hidden', () => {
		useCompanionStore.setState({ state: 'hidden' });
		useCompanionStore.getState().cycleState();
		expect(useCompanionStore.getState().state).toBe('collapsed');
	});

	it('an incoming permission request expands without touching focus', () => {
		useCompanionStore.getState().receivePermission(req('r1'));
		const s = useCompanionStore.getState();
		expect(s.state).toBe('expanded');
		expect(s.focusPending).toBe(false);
		expect(s.permissions[0]).toMatchObject({ id: 'r1', status: 'pending' });
	});

	it('after the user collapses with a request pending, new requests stay quiet for 60 s', () => {
		const st = useCompanionStore.getState;
		st().receivePermission(req('r1'));
		st().setState('collapsed');
		st().receivePermission(req('r2'));
		expect(st().state).toBe('collapsed');
		vi.advanceTimersByTime(PERMISSION_QUIET_MS - 1);
		st().receivePermission(req('r3'));
		expect(st().state).toBe('collapsed');
		vi.advanceTimersByTime(1);
		st().receivePermission(req('r4'));
		expect(st().state).toBe('expanded');
	});

	it('collapsing with nothing pending does not start a quiet period', () => {
		const st = useCompanionStore.getState;
		st().setState('expanded');
		st().setState('collapsed');
		st().receivePermission(req('r1'));
		expect(st().state).toBe('expanded');
	});

	it('"Hand to Chi" expands, pre-fills the dispatch input and focuses it', () => {
		handToChi('Look at src/app.ts');
		const s = useCompanionStore.getState();
		expect(s.state).toBe('expanded');
		expect(s.draft).toBe('Look at src/app.ts');
		expect(s.focusPending).toBe(true);
	});

	it('a tab dropped into the Companion expands and selects it (scope + target)', () => {
		useCompanionStore.getState().appendView({ kind: 'terminal', sessionId: 'term-a' });
		const s = useCompanionStore.getState();
		expect(s.state).toBe('expanded');
		expect(s.tabs).toEqual([{ kind: 'terminal', sessionId: 'term-a' }]);
		expect(s.panelScopeSessionId).toBe('term-a');
		expect(useShellStore.getState().companion.activeTarget).toEqual({
			kind: 'session',
			session_id: 'term-a',
		});
	});

	it('re-dropping the same session selects the existing tab instead of duplicating it', () => {
		const st = useCompanionStore.getState;
		st().appendView({ kind: 'terminal', sessionId: 'a' });
		st().appendView({ kind: 'terminal', sessionId: 'b' });
		st().appendView({ kind: 'terminal', sessionId: 'a' });
		expect(st().tabs).toHaveLength(2);
		expect(st().activeIdx).toBe(0);
	});
});

describe('session tabs', () => {
	it('selecting a tab sets both the panel scope and companion.activeTarget', () => {
		useCompanionStore.setState({
			tabs: [
				{ kind: 'terminal', sessionId: 'a' },
				{ kind: 'terminal', sessionId: 'b' },
			],
		});
		useCompanionStore.getState().selectSession(1);
		expect(useCompanionStore.getState().panelScopeSessionId).toBe('b');
		expect(useShellStore.getState().companion.activeTarget).toEqual({
			kind: 'session',
			session_id: 'b',
		});
	});

	it('closing the scoped tab clears the scope (the PTY is not touched)', () => {
		useCompanionStore.setState({
			tabs: [{ kind: 'terminal', sessionId: 'a' }],
			panelScopeSessionId: 'a',
		});
		useCompanionStore.getState().closeTab(0);
		expect(useCompanionStore.getState().tabs).toEqual([]);
		expect(useCompanionStore.getState().panelScopeSessionId).toBeNull();
	});
});

describe('C3 — permission card resolve + undo (§5.6)', () => {
	it('a decision is held for the 5 s undo window, then posted', async () => {
		const st = useCompanionStore.getState;
		st().receivePermission(req('r1'));
		st().resolvePermission('r1', 'allow');
		expect(st().permissions[0]).toMatchObject({ status: 'undoable', decision: 'allow' });
		expect(iykeFetch).not.toHaveBeenCalled();

		await vi.advanceTimersByTimeAsync(PERMISSION_UNDO_MS - 1);
		expect(iykeFetch).not.toHaveBeenCalled();

		await vi.advanceTimersByTimeAsync(1);
		expect(iykeFetch).toHaveBeenCalledTimes(1);
		expect(iykeFetch).toHaveBeenCalledWith('/iyke/hooks/decision', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ requestId: 'r1', decision: 'approved' }),
		});
		expect(st().permissions[0]).toMatchObject({ status: 'resolved', decision: 'allow' });
	});

	it('Undo inside the window returns the card to pending and nothing is posted', async () => {
		const st = useCompanionStore.getState;
		st().receivePermission(req('r1'));
		st().resolvePermission('r1', 'deny');
		await vi.advanceTimersByTimeAsync(PERMISSION_UNDO_MS - 100);
		st().undoPermission('r1');
		expect(st().permissions[0]).toMatchObject({ status: 'pending', decision: undefined });
		await vi.advanceTimersByTimeAsync(PERMISSION_UNDO_MS * 2);
		expect(iykeFetch).not.toHaveBeenCalled();

		// …and it can be decided again afterwards.
		st().resolvePermission('r1', 'deny');
		await vi.advanceTimersByTimeAsync(PERMISSION_UNDO_MS);
		expect(iykeFetch).toHaveBeenCalledWith(
			'/iyke/hooks/decision',
			expect.objectContaining({ body: JSON.stringify({ requestId: 'r1', decision: 'denied' }) })
		);
	});

	it('Undo after the window has closed is a no-op', async () => {
		const st = useCompanionStore.getState;
		st().receivePermission(req('r1'));
		st().resolvePermission('r1', 'allow');
		await vi.advanceTimersByTimeAsync(PERMISSION_UNDO_MS);
		st().undoPermission('r1');
		expect(st().permissions[0].status).toBe('resolved');
		expect(iykeFetch).toHaveBeenCalledTimes(1);
	});

	it('a decision consumed elsewhere first replaces Undo with "Already applied"', async () => {
		const st = useCompanionStore.getState;
		st().receivePermission(req('r1'));
		st().resolvePermission('r1', 'allow');
		st().permissionDecided('r1', 'denied'); // e.g. the gate timed out
		expect(st().permissions[0]).toMatchObject({
			status: 'resolved',
			appliedElsewhere: true,
			decision: 'deny',
		});
		st().undoPermission('r1');
		expect(st().permissions[0].status).toBe('resolved');
		await vi.advanceTimersByTimeAsync(PERMISSION_UNDO_MS * 2);
		expect(iykeFetch).not.toHaveBeenCalled();
	});

	it('"Always for this project" writes the rule on commit, never inside the window', async () => {
		useShellStore.setState({
			activeProject: { id: 'p1', root_path: '/work/royalti-co', extra_roots: [] },
		});
		fsRead.mockResolvedValue({
			bytes: Array.from(new TextEncoder().encode('{"permissions":{"allow":["Read"]}}')),
			mime: 'application/json',
		});
		const st = useCompanionStore.getState;
		st().receivePermission(req('r1'));
		st().resolvePermission('r1', 'always');
		await vi.advanceTimersByTimeAsync(PERMISSION_UNDO_MS - 1);
		expect(fsWriteText).not.toHaveBeenCalled();
		await vi.advanceTimersByTimeAsync(1);
		expect(fsWriteText).toHaveBeenCalledWith(
			'/work/royalti-co/.claude/settings.json',
			`${JSON.stringify({ permissions: { allow: ['Read', 'Bash'] } }, null, 2)}\n`
		);
		expect(iykeFetch).toHaveBeenCalledWith(
			'/iyke/hooks/decision',
			expect.objectContaining({ body: JSON.stringify({ requestId: 'r1', decision: 'approved' }) })
		);
		expect(st().permissions[0].ruleFile).toBe('/work/royalti-co/.claude/settings.json');
	});

	it('keeps the last 20 cards', () => {
		const st = useCompanionStore.getState;
		for (let i = 0; i < 25; i++) st().receivePermission(req(`r${i}`));
		expect(st().permissions).toHaveLength(20);
		expect(st().permissions[0].id).toBe('r24');
	});
});
