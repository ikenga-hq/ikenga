// Companion store — the right-hand Chi Companion (was the Dock; WP-06, D-01).
//
// Owns the Companion's own UI state: strip ↔ expanded, width, the session
// tabs (parked terminal views — still `PaneView`s so tab drag between the
// Companion and the pane tree keeps working), the panel scope, the dispatch
// draft + focus requests, and the §5.6 permission queue with its undo window.
//
// The dispatch *target* is NOT here: it is G-STATE's
// `useShellStore.companion.activeTarget` (drafts/g-state.md). Selecting a
// session tab sets both the panel scope (here) and the target (there).
//
// The Companion renders state, never model prose (ADR-021). Nothing in this
// store holds a response, a transcript or streamed text.

import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import { iykeFetch } from '@/lib/iyke/client';
import type { PaneView } from '@/lib/panes/types';
import { useShellStore } from '@/lib/shell/shell-store';
import { fsRead, fsWriteText } from '@/lib/tauri-cmd';
import { scopedPersistName } from '@/lib/window/window-context';

export type CompanionState = 'collapsed' | 'expanded' | 'hidden';

export const COMPANION_MIN_WIDTH = 280;
export const COMPANION_MAX_WIDTH = 900;
export const COMPANION_DEFAULT_WIDTH = 372;

/** §5.6 — the undo window after a permission decision. The decision is held
 *  client-side for this long before it is posted, so "the engine has not yet
 *  acted" is true for the whole window by construction. */
export const PERMISSION_UNDO_MS = 5_000;
/** §5.2 — after the user collapses the Companion while a request is pending,
 *  further requests do not re-expand it for this long. */
export const PERMISSION_QUIET_MS = 60_000;
/** §5.6.6 — resolved cards kept for the session. */
export const PERMISSION_KEEP = 20;

export type PermissionDecision = 'allow' | 'always' | 'deny';

export interface PermissionCardEntry {
	/** The hooks bus request id (what `/iyke/hooks/decision` takes). */
	id: string;
	kind: 'permission' | 'tool_use';
	toolName: string;
	toolInput?: Record<string, unknown>;
	/** Short request text from the hook payload (the engine's ask, not prose). */
	prompt?: string;
	/** `ikenga_terminal_id` of the session that asked, when known. */
	sessionId?: string;
	arrivedAt: number;
	/** pending → undoable (decided, inside the 5 s window) → resolved. */
	status: 'pending' | 'undoable' | 'resolved';
	decision?: PermissionDecision;
	undoUntil?: number;
	/** The engine consumed a decision before ours landed (another surface or
	 *  the gate timeout) — the Undo control reads "Already applied". */
	appliedElsewhere?: boolean;
	/** Settings file an "Always for this project" decision wrote. */
	ruleFile?: string;
	error?: string;
}

interface CompanionStoreState {
	state: CompanionState;
	/** Session tabs. Named `tabs` for continuity with the drag machinery
	 *  (`drag-state.ts` `startDock(idx)` indexes into it). */
	tabs: PaneView[];
	activeIdx: number;
	width: number;

	/** Which session the scoped panels (cost, tool feed, permission inbox)
	 *  show. `null` → their empty state. Not persisted: session ids die with
	 *  the app, exactly like G-STATE's `activeTarget`. */
	panelScopeSessionId: string | null;
	/** Dispatch input text. Lives here so "Hand to Chi" can pre-fill it. */
	draft: string;
	/** Something asked for the dispatch input to take focus; the dispatch bar
	 *  consumes it (on mount too — the request usually expands the Companion,
	 *  so the input does not exist yet when it is made). */
	focusPending: boolean;
	/** Something asked for the target picker to open; the picker consumes it. */
	pickerPending: boolean;

	permissions: PermissionCardEntry[];
	/** When the user last collapsed the Companion while a request was pending. */
	quietSince: number | null;

	setState: (s: CompanionState) => void;
	toggleExpanded: () => void;
	cycleState: () => void;
	setWidth: (n: number) => void;

	addTab: (view: PaneView) => void;
	closeTab: (idx: number) => void;
	switchTab: (idx: number) => void;
	/** Select a session tab: sets the panel scope AND `companion.activeTarget`. */
	selectSession: (idx: number) => void;
	togglePinned: (idx: number) => void;
	/** Pull a view in (e.g., from a pane drop). Expands and selects it. */
	appendView: (view: PaneView) => void;

	setPanelScope: (sessionId: string | null) => void;
	setDraft: (text: string) => void;
	/** Expand and move focus to the dispatch input (⌘2, ⌘J-expand, ⌘⇧A). */
	focusDispatch: () => void;
	/** Expand and open the target picker (the scoped panels' empty action). */
	openTargetPicker: () => void;
	consumeFocus: () => void;
	consumePicker: () => void;

	receivePermission: (
		entry: Omit<PermissionCardEntry, 'status' | 'arrivedAt'> & {
			arrivedAt?: number;
		}
	) => void;
	resolvePermission: (id: string, decision: PermissionDecision) => void;
	undoPermission: (id: string) => void;
	/** The hooks bus reported a decision for `id` (ours or anyone's). */
	permissionDecided: (id: string, decision: string) => void;
}

// The strip ↔ expanded toggle. `hidden` is deliberately NOT in the cycle: it
// renders nothing at all, so cycling into it strands the Companion with no
// affordance to get it back. It stays a programmatic state (`setState`).
const STATE_CYCLE: CompanionState[] = ['collapsed', 'expanded'];

const clampWidth = (n: number) =>
	Math.max(COMPANION_MIN_WIDTH, Math.min(COMPANION_MAX_WIDTH, Math.round(n)));

// Commit timers for decisions inside their undo window. Module-level: they
// are not state, and must never be persisted.
const commitTimers = new Map<string, ReturnType<typeof setTimeout>>();

function clearCommitTimer(id: string) {
	const t = commitTimers.get(id);
	if (t) clearTimeout(t);
	commitTimers.delete(id);
}

function hasPending(perms: PermissionCardEntry[]) {
	return perms.some((p) => p.status === 'pending');
}

function scopeFor(view: PaneView | undefined): string | null {
	return view?.kind === 'terminal' ? view.sessionId : null;
}

async function postDecision(requestId: string, decision: 'approved' | 'denied') {
	await iykeFetch('/iyke/hooks/decision', {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ requestId, decision }),
	});
}

/** "Always for this project" — add the tool to `permissions.allow` in
 *  `<project>/.claude/settings.json`. Returns the file written. */
async function writeProjectAllowRule(toolName: string): Promise<string> {
	const root = useShellStore.getState().activeProject.root_path;
	if (!root) throw new Error('No project folder to write a rule into');
	const sep = root.includes('\\') && !root.includes('/') ? '\\' : '/';
	const file = `${root.replace(/[\\/]+$/, '')}${sep}.claude${sep}settings.json`;
	let settings: Record<string, unknown> = {};
	try {
		const res = await fsRead(file);
		const text = new TextDecoder().decode(new Uint8Array(res.bytes));
		if (text.trim()) settings = JSON.parse(text) as Record<string, unknown>;
	} catch (e) {
		// Missing file → start fresh; unparseable file → refuse rather than clobber.
		if (e instanceof SyntaxError) throw new Error(`${file} is not valid JSON`);
	}
	const perms = (settings.permissions ?? {}) as Record<string, unknown>;
	const allow = Array.isArray(perms.allow) ? (perms.allow as unknown[]) : [];
	if (!allow.includes(toolName)) allow.push(toolName);
	settings.permissions = { ...perms, allow };
	await fsWriteText(file, `${JSON.stringify(settings, null, 2)}\n`);
	return file;
}

export const useCompanionStore = create<CompanionStoreState>()(
	persist(
		(set, get) => {
			const patchCard = (id: string, patch: Partial<PermissionCardEntry>) =>
				set((s) => ({
					permissions: s.permissions.map((p) => (p.id === id ? { ...p, ...patch } : p)),
				}));

			async function commit(id: string) {
				commitTimers.delete(id);
				const card = get().permissions.find((p) => p.id === id);
				if (!card || card.status !== 'undoable' || !card.decision) return;
				patchCard(id, { status: 'resolved', undoUntil: undefined });
				try {
					let ruleFile: string | undefined;
					if (card.decision === 'always') ruleFile = await writeProjectAllowRule(card.toolName);
					await postDecision(id, card.decision === 'deny' ? 'denied' : 'approved');
					if (ruleFile) patchCard(id, { ruleFile });
				} catch (e) {
					patchCard(id, { error: e instanceof Error ? e.message : String(e) });
				}
			}

			return {
				state: 'collapsed',
				tabs: [],
				activeIdx: 0,
				width: COMPANION_DEFAULT_WIDTH,
				panelScopeSessionId: null,
				draft: '',
				focusPending: false,
				pickerPending: false,
				permissions: [],
				quietSince: null,

				setState: (state) =>
					set((s) => ({
						state,
						quietSince:
							state === 'collapsed' && s.state === 'expanded' && hasPending(s.permissions)
								? Date.now()
								: s.quietSince,
					})),
				toggleExpanded: () => get().setState(get().state === 'expanded' ? 'collapsed' : 'expanded'),
				cycleState: () => {
					const i = STATE_CYCLE.indexOf(get().state);
					const next = STATE_CYCLE[(i + 1) % STATE_CYCLE.length];
					get().setState(next);
					// §5.2: ⌘J lands focus on the dispatch input when it expands.
					if (next === 'expanded') set({ focusPending: true });
				},
				setWidth: (n) => set({ width: clampWidth(n) }),

				addTab: (view) => get().appendView(view),
				closeTab: (idx) =>
					set((s) => {
						const closed = s.tabs[idx];
						const tabs = s.tabs.filter((_, i) => i !== idx);
						const activeIdx = tabs.length === 0 ? 0 : Math.min(s.activeIdx, tabs.length - 1);
						// Closing the scoped session's tab clears the scope; it never
						// kills the PTY (spec §3.11 #66).
						const scopeGone = scopeFor(closed) === s.panelScopeSessionId;
						return {
							tabs,
							activeIdx,
							panelScopeSessionId: scopeGone ? null : s.panelScopeSessionId,
						};
					}),
				switchTab: (idx) => set({ activeIdx: idx }),
				selectSession: (idx) => {
					const view = get().tabs[idx];
					if (!view) return;
					const scope = scopeFor(view);
					set({ activeIdx: idx, panelScopeSessionId: scope });
					if (scope) {
						useShellStore.getState().setCompanionTarget({ kind: 'session', session_id: scope });
					}
				},
				togglePinned: (idx) =>
					set((s) => ({
						tabs: s.tabs.map((t, i) => (i === idx ? { ...t, pinned: !t.pinned } : t)),
					})),
				appendView: (view) => {
					const s = get();
					// A session is one tab: re-dropping the same terminal selects it.
					const existing =
						view.kind === 'terminal'
							? s.tabs.findIndex((t) => t.kind === 'terminal' && t.sessionId === view.sessionId)
							: -1;
					if (existing < 0) set({ tabs: [...s.tabs, view] });
					set({ state: 'expanded' });
					get().selectSession(existing >= 0 ? existing : get().tabs.length - 1);
				},

				setPanelScope: (panelScopeSessionId) => set({ panelScopeSessionId }),
				setDraft: (draft) => set({ draft }),
				focusDispatch: () => set({ state: 'expanded', focusPending: true }),
				openTargetPicker: () => set({ state: 'expanded', pickerPending: true }),
				consumeFocus: () => set({ focusPending: false }),
				consumePicker: () => set({ pickerPending: false }),

				receivePermission: (entry) => {
					const s = get();
					if (s.permissions.some((p) => p.id === entry.id)) return;
					const card: PermissionCardEntry = {
						...entry,
						arrivedAt: entry.arrivedAt ?? Date.now(),
						status: 'pending',
					};
					const now = Date.now();
					const quiet = s.quietSince != null && now - s.quietSince < PERMISSION_QUIET_MS;
					set({
						permissions: [card, ...s.permissions].slice(0, PERMISSION_KEEP),
						// §5.2: auto-expand once per quiet period. Never touches focus.
						state: s.state !== 'expanded' && !quiet ? 'expanded' : s.state,
					});
				},
				resolvePermission: (id, decision) => {
					const card = get().permissions.find((p) => p.id === id);
					if (!card || card.status !== 'pending') return;
					clearCommitTimer(id);
					patchCard(id, {
						status: 'undoable',
						decision,
						undoUntil: Date.now() + PERMISSION_UNDO_MS,
					});
					commitTimers.set(
						id,
						setTimeout(() => void commit(id), PERMISSION_UNDO_MS)
					);
				},
				undoPermission: (id) => {
					const card = get().permissions.find((p) => p.id === id);
					if (!card || card.status !== 'undoable' || card.appliedElsewhere) return;
					clearCommitTimer(id);
					patchCard(id, { status: 'pending', decision: undefined, undoUntil: undefined });
				},
				permissionDecided: (id, decision) => {
					const card = get().permissions.find((p) => p.id === id);
					if (!card || card.status === 'resolved') return;
					// Someone else's decision (or the gate timeout) reached the engine
					// first: ours can no longer apply, and Undo would be a lie.
					clearCommitTimer(id);
					patchCard(id, {
						status: 'resolved',
						undoUntil: undefined,
						appliedElsewhere: true,
						decision: decision === 'approved' ? (card.decision ?? 'allow') : 'deny',
					});
				},
			};
		},
		{
			// Window-namespaced (plans/multi-window WP-05). Name kept from the
			// Dock era so existing users keep their width / parked tabs, and so
			// Settings → Clear data (`clear-data.tsx`) still finds it.
			name: scopedPersistName('ikenga-dock'),
			version: 4,
			partialize: (s) => ({
				state: s.state,
				tabs: s.tabs,
				activeIdx: s.activeIdx,
				width: s.width,
			}),
			migrate: (persisted: unknown, version) => {
				const p = (persisted ?? {}) as Record<string, unknown>;
				if (version < 2) {
					// v1 had a 'wide' state — collapse it into 'expanded'.
					if (p.state === 'wide') p.state = 'expanded';
					if (p.width == null) p.width = COMPANION_DEFAULT_WIDTH;
				}
				if (version < 3) {
					// v2 could cycle into 'hidden', which renders nothing.
					if (p.state === 'hidden') p.state = 'collapsed';
				}
				if (version < 4) {
					// v4 (WP-06): the Dock became the Companion, whose minimum is
					// wider than the Dock's 240 px.
					if (typeof p.width === 'number') p.width = clampWidth(p.width);
				}
				return p as unknown as CompanionStoreState;
			},
		}
	)
);

/** Expand the Companion and focus its dispatch input — what ⌘2 / the
 *  `ikenga:companion-focus` window event do (spec §5.2). */
export function focusCompanion(): void {
	useCompanionStore.getState().focusDispatch();
}

/**
 * "Hand to Chi" — a NEW action (none existed before WP-06), exported for
 * WP-04's context menus. Expands the Companion, pre-fills the dispatch input
 * with the handed-off context and focuses it (spec §5.2). It never sends:
 * the user reads, edits and presses Enter.
 */
export function handToChi(text: string): void {
	const s = useCompanionStore.getState();
	s.setDraft(text);
	s.focusDispatch();
}

/** Test seam: drop every pending undo timer. */
export function __resetCompanionTimersForTests(): void {
	for (const id of Array.from(commitTimers.keys())) clearCommitTimer(id);
}
