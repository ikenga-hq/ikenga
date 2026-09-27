// WP-67 — the one read the seat rail, the rest strip, the target picker and
// the scoped panels share: the active project's seats (G-SEATS §9.2
// `seats_list` through `useSeats`), the sessions no seat holds (the
// "Unseated" group, §11.2: "WP-67 renders the group from sessions with no
// seat"), and the pending permission count per session.
//
// The seats list is overlaid with the rail's own pending Undo windows:
// a seat inside its 8 s *Remove* window is hidden, and one inside its *Clear*
// window reads vacant with no history (§4.2 — the host call is made when the
// window closes).

import { useMemo } from 'react';
import { useSeats } from '@/lib/queries/seats';
import { useShellStore } from '@/lib/shell/shell-store';
import type { SeatView } from '@/lib/tauri-cmd';
import { type TerminalTab, useTerminalStore } from '@/terminal/session-store';
import { useCompanionStore } from './companion-store';
import { useSeatUi } from './seat-actions';
import { chiEngineForWrap } from './seat-model';

export interface UnseatedSession {
	/** Terminal tab id (= the pane view's `sessionId`). */
	id: string;
	/** Chi engine id, or null for a plain shell / an engine with no Chi id. */
	engineId: string | null;
	/** Index in the Companion's parked `tabs` (a pane drop), if parked there. */
	parkedIdx: number | null;
	status: TerminalTab['status'];
	title: string;
	cwd: string;
	/** The engine's resume id, when the agent reported one. */
	externalId: string | null;
}

export type RosterState = 'loading' | 'error' | 'ready';

export interface SeatRoster {
	projectId: string;
	seats: SeatView[];
	state: RosterState;
	error: string | null;
	unseated: UnseatedSession[];
	/** Pending permission requests per session id (terminal id). */
	pendingBySession: Record<string, number>;
	/** Pending requests with no session attribution (shown everywhere). */
	pendingUnattributed: number;
}

/** A seat inside its Clear window: no session, vacant, nothing to resume. */
function clearedView(seat: SeatView): SeatView {
	return {
		...seat,
		session: null,
		status: 'vacant',
		agent: null,
		mount: null,
		resume: { resumable: false, reason: 'no_session' },
	};
}

export function seatedTerminalIds(seats: readonly SeatView[]): Set<string> {
	const out = new Set<string>();
	for (const s of seats) if (s.session?.kind === 'terminal') out.add(s.session.terminal_id);
	return out;
}

/** The engine a terminal runs, in the Chi id space (§4.3). */
export function terminalEngine(tab: Pick<TerminalTab, 'spec' | 'claudeSessionId'>): string | null {
	const wrap = tab.spec.wrap?.engine ?? (tab.spec.wrap || tab.claudeSessionId ? 'claude' : null);
	return chiEngineForWrap(wrap);
}

export function useSeatRoster(): SeatRoster {
	const projectId = useShellStore((s) => s.activeProject.id);
	const query = useSeats(projectId ?? null);
	const removing = useSeatUi((s) => s.removing);
	const clearing = useSeatUi((s) => s.clearing);
	const terminals = useTerminalStore((s) => s.tabs);
	const parked = useCompanionStore((s) => s.tabs);
	const permissions = useCompanionStore((s) => s.permissions);

	const seats = useMemo(
		() =>
			(query.data ?? [])
				.filter((s) => !removing[s.id])
				.map((s) => (clearing[s.id] ? clearedView(s) : s)),
		[query.data, removing, clearing]
	);

	const unseated = useMemo(() => {
		// Seats inside their Remove window still hold their session until the
		// host call lands — but the locked rail shows it unseated at once.
		const held = seatedTerminalIds(seats);
		const parkedIdx = new Map<string, number>();
		parked.forEach((v, i) => {
			if (v.kind === 'terminal' && !parkedIdx.has(v.sessionId)) parkedIdx.set(v.sessionId, i);
		});
		const out: UnseatedSession[] = [];
		for (const tab of terminals) {
			if (held.has(tab.id)) continue;
			const isParked = parkedIdx.has(tab.id);
			const isAgent = Boolean(tab.spec.wrap);
			const isLive = tab.status === 'running' || tab.status === 'spawning';
			// An agent session while it runs; any terminal parked in the Companion.
			if (!isParked && !(isAgent && isLive)) continue;
			out.push({
				id: tab.id,
				engineId: terminalEngine(tab),
				parkedIdx: parkedIdx.get(tab.id) ?? null,
				status: tab.status,
				title: tab.title,
				cwd: tab.spec.cwd,
				externalId: tab.claudeSessionId ?? null,
			});
		}
		// A parked view whose terminal the store no longer has (restored from
		// an older run) stays listed, as its session tab was — never lost.
		for (const [id, idx] of parkedIdx) {
			if (held.has(id) || out.some((u) => u.id === id)) continue;
			out.push({ id, engineId: null, parkedIdx: idx, status: 'exited', title: '', cwd: '', externalId: null });
		}
		return out.sort((a, b) => {
			const ta = terminals.find((t) => t.id === a.id)?.createdAt ?? Number.MAX_SAFE_INTEGER;
			const tb = terminals.find((t) => t.id === b.id)?.createdAt ?? Number.MAX_SAFE_INTEGER;
			return ta - tb;
		});
	}, [seats, terminals, parked]);

	const { pendingBySession, pendingUnattributed } = useMemo(() => {
		const by: Record<string, number> = {};
		let none = 0;
		for (const p of permissions) {
			if (p.status !== 'pending') continue;
			if (p.sessionId) by[p.sessionId] = (by[p.sessionId] ?? 0) + 1;
			else none += 1;
		}
		return { pendingBySession: by, pendingUnattributed: none };
	}, [permissions]);

	const state: RosterState = query.isError ? 'error' : query.data ? 'ready' : 'loading';
	const error = query.error
		? query.error instanceof Error
			? query.error.message
			: typeof (query.error as { message?: unknown }).message === 'string'
				? (query.error as { message: string }).message
				: String(query.error)
		: null;

	return { projectId, seats, state, error, unseated, pendingBySession, pendingUnattributed };
}
