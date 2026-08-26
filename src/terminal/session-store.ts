/**
 * session-store — Zustand store for the terminal panel.
 *
 * Persists the *list* of tabs (without PTY ids — those die with the app) to
 * SQLite via the @tauri-apps/plugin-sql plugin. If the SQL plugin isn't ready
 * (e.g. rust-eng hasn't installed it yet), we fall back to localStorage.
 *
 * PTY lifecycle itself lives in pty-bridge.ts; this store only tracks
 * `ptyId` strings as opaque references.
 */

import { create } from 'zustand';

const STORAGE_KEY = 'terminal.tabs';
const SQL_DB_URL = 'sqlite:ikenga-terminal.sqlite';

/** Who currently owns the xterm renderer for this tab.
 *
 * - `sidepane` — default. The side-pane Terminal panel mounts the xterm.
 * - `studio` — an Artifact Studio loupe pane has taken the xterm; the side
 *   pane shows a placeholder body and the tab strip entry stays visible
 *   (so the user can navigate back). The owning Studio pane is keyed by
 *   `paneId`. `artifactPath` is recorded for the placeholder copy. */
export type TerminalOwner =
	| { kind: 'sidepane' }
	| { kind: 'studio'; paneId: string; artifactPath: string };

export interface TerminalTab {
	id: string;
	title: string;
	spec: { cwd: string; cmd: string[]; env?: Record<string, string> };
	ptyId: string | null;
	status: 'spawning' | 'running' | 'exited' | 'error';
	exitCode: number | null;
	createdAt: number;
	owner: TerminalOwner;
	wasRunning?: boolean;
}

interface TerminalState {
	tabs: TerminalTab[];
	activeId: string | null;
	rehydrated: boolean;

	add: (spec: TerminalTab['spec'], title?: string) => string;
	setActive: (id: string) => void;
	remove: (id: string) => void;
	rename: (id: string, title: string) => void;
	setPtyId: (id: string, ptyId: string | null) => void;
	setStatus: (id: string, status: TerminalTab['status'], exitCode?: number | null) => void;

	/** Attempt to attach `tabId` to an Artifact Studio pane. If the tab is
	 *  currently owned by another Studio pane, returns
	 *  `{ ok: false, requiresConfirm: true, previousPaneId }` so the caller
	 *  can show the "reclaim from pane X?" prompt. Pass `{ force: true }`
	 *  to override unconditionally (the user confirmed). */
	attachToStudio: (
		tabId: string,
		paneId: string,
		artifactPath: string,
		opts?: { force?: boolean }
	) => { ok: true } | { ok: false; requiresConfirm: true; previousPaneId: string };
	/** Restore ownership to the side pane. Idempotent. */
	detachFromStudio: (tabId: string) => void;
	/** Return the tab attached to `paneId`, if any. */
	findStudioAttachment: (paneId: string) => TerminalTab | null;

	rehydrateFromDb: () => Promise<void>;
	persistToDb: () => Promise<void>;
}

// --- persistence helpers ---------------------------------------------------

interface SerializedTab {
	id: string;
	title: string;
	spec: TerminalTab['spec'];
	status: TerminalTab['status'];
	wasRunning?: boolean;
	exitCode: number | null;
	createdAt: number;
	owner?: TerminalOwner;
}

/** ADR-013 §Addendum Decision 3 — drop credential-shaped env vars before a
 *  terminal tab's `spec.env` is persisted to SQLite/localStorage. Today
 *  nothing routes secrets through `spec.env`, but the restored tab is a
 *  durable on-disk record, so we strip defensively: any key matching a
 *  credential pattern (API keys, tokens, secrets, passwords, AWS creds)
 *  never reaches the persisted blob. Mirrors cmux's "strip secrets from
 *  captured env before saving resume state." */
const SECRET_ENV_PATTERN =
	/(_|^)(API_?KEY|TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL|ACCESS_KEY|PRIVATE_KEY|SESSION_TOKEN|REFRESH_TOKEN|CLIENT_SECRET)S?(_|$)/i;

export function stripSecretEnv(
	env: Record<string, string> | undefined
): Record<string, string> | undefined {
	if (!env) return env;
	let stripped = false;
	const clean: Record<string, string> = {};
	for (const [k, v] of Object.entries(env)) {
		if (SECRET_ENV_PATTERN.test(k)) {
			stripped = true;
			continue;
		}
		clean[k] = v;
	}
	// Preserve `undefined` vs `{}` distinction only when we actually removed
	// something or there were keys to begin with.
	return stripped || Object.keys(env).length > 0 ? clean : env;
}

function serialize(tabs: TerminalTab[]): SerializedTab[] {
	return tabs.map(({ id, title, spec, status, exitCode, createdAt, owner, wasRunning: prevWasRunning }) => {
		const isCurrentlyRunning = status === 'running' || status === 'spawning';
		const wasRunning = isCurrentlyRunning || Boolean(prevWasRunning);
		return {
			id,
			title,
			// Strip credential-shaped env vars before persisting (ADR-013
			// §Addendum Decision 3) — the restored tab is a durable on-disk record.
			spec: { ...spec, env: stripSecretEnv(spec.env) },
			// ptyIds are runtime-only; restored tabs start spawning if previously running, else exited.
			status: isCurrentlyRunning ? 'exited' : status,
			wasRunning,
			exitCode,
			createdAt,
			owner,
		};
	});
}

type SqlDb = {
	execute: (sql: string, params?: unknown[]) => Promise<unknown>;
	select: <T = unknown>(sql: string, params?: unknown[]) => Promise<T>;
};

let cachedDb: SqlDb | null = null;
let dbLoadAttempted = false;
let dbAvailable = false;

async function loadDb(): Promise<SqlDb | null> {
	if (cachedDb) return cachedDb;
	if (dbLoadAttempted && !dbAvailable) return null;
	dbLoadAttempted = true;
	try {
		const mod = await import('@/lib/transport/sql-shim');
		const Database = (mod as unknown as { default: { load: (url: string) => Promise<SqlDb> } })
			.default;
		const db = await Database.load(SQL_DB_URL);
		// Best-effort table init. If layout_state already exists, this is a no-op.
		await db.execute(
			'CREATE TABLE IF NOT EXISTS layout_state (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL)'
		);
		cachedDb = db;
		dbAvailable = true;
		return db;
	} catch (err) {
		console.warn(
			'[terminal/session-store] SQL plugin unavailable, falling back to localStorage',
			err
		);
		dbAvailable = false;
		return null;
	}
}

async function readPersisted(): Promise<SerializedTab[]> {
	const db = await loadDb();
	if (db) {
		try {
			const rows = await db.select<{ value: string }[]>(
				'SELECT value FROM layout_state WHERE key = $1',
				[STORAGE_KEY]
			);
			if (rows && rows.length > 0) {
				return JSON.parse(rows[0].value) as SerializedTab[];
			}
			return [];
		} catch (err) {
			console.warn('[terminal/session-store] read failed, falling back', err);
		}
	}
	// localStorage fallback.
	try {
		const raw = localStorage.getItem(STORAGE_KEY);
		if (!raw) return [];
		return JSON.parse(raw) as SerializedTab[];
	} catch {
		return [];
	}
}

async function writePersisted(tabs: SerializedTab[]): Promise<void> {
	const json = JSON.stringify(tabs);
	const db = await loadDb();
	if (db) {
		try {
			await db.execute(
				'INSERT INTO layout_state (key, value, updated_at) VALUES ($1, $2, $3) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at',
				[STORAGE_KEY, json, Date.now()]
			);
			return;
		} catch (err) {
			console.warn('[terminal/session-store] write failed, falling back', err);
		}
	}
	try {
		localStorage.setItem(STORAGE_KEY, json);
	} catch {
		/* ignore */
	}
}

// --- debounce helper -------------------------------------------------------

function debounce<A extends unknown[]>(fn: (...args: A) => void, ms: number): (...args: A) => void {
	let t: ReturnType<typeof setTimeout> | null = null;
	return (...args) => {
		if (t) clearTimeout(t);
		t = setTimeout(() => fn(...args), ms);
	};
}

/** Clear a specific Studio pane's `attachedTerminalId`. Cross-store helper
 *  used by attach/detach when the previous owner needs to forget the tab.
 *  Lazy-imports pane-store to dodge a cycle. */
function clearPaneAttachment(paneId: string): void {
	void import('@/lib/panes/pane-store').then(({ usePaneStore }) => {
		usePaneStore.getState().setStudioAttachedTerminal(paneId, null);
	});
}

// --- store -----------------------------------------------------------------

let nextSeq = 0;
function makeId(): string {
	// Minimal uuid-ish — uses crypto.randomUUID() if present, else seq+rand.
	const c = (globalThis as { crypto?: { randomUUID?: () => string } }).crypto;
	if (c?.randomUUID) return c.randomUUID();
	nextSeq += 1;
	return `tab-${Date.now()}-${nextSeq}-${Math.random().toString(36).slice(2, 8)}`;
}

export const useTerminalStore = create<TerminalState>((set, get) => {
	const persistDebounced = debounce(() => {
		void writePersisted(serialize(get().tabs));
	}, 300);

	return {
		tabs: [],
		activeId: null,
		rehydrated: false,

		add: (spec, title) => {
			const id = makeId();
			const tab: TerminalTab = {
				id,
				title: title ?? spec.cmd[0] ?? 'shell',
				spec,
				ptyId: null,
				status: 'spawning',
				exitCode: null,
				createdAt: Date.now(),
				owner: { kind: 'sidepane' },
			};
			set((s) => ({ tabs: [...s.tabs, tab], activeId: id }));
			persistDebounced();
			return id;
		},

		setActive: (id) => {
			set({ activeId: id });
		},

		remove: (id) => {
			set((s) => {
				const tabs = s.tabs.filter((t) => t.id !== id);
				const activeId = s.activeId === id ? (tabs[tabs.length - 1]?.id ?? null) : s.activeId;
				return { tabs, activeId };
			});
			persistDebounced();
			// Clear any `artifact-studio` view referencing the removed tab so
			// the Studio rail falls back to its picker instead of holding a
			// stale id. Cross-store; lazy import to dodge cycles.
			void import('@/lib/panes/pane-store').then(({ usePaneStore }) => {
				const ps = usePaneStore.getState();
				const visit = (node: import('@/lib/panes/types').PaneNode): void => {
					if (node.type === 'leaf') {
						for (const tab of node.tabs) {
							if (tab.kind === 'artifact-studio' && tab.attachedTerminalId === id) {
								ps.setStudioAttachedTerminal(node.id, null);
							}
						}
					} else {
						for (const c of node.children) visit(c);
					}
				};
				visit(ps.root);
			});
		},

		rename: (id, title) => {
			set((s) => ({
				tabs: s.tabs.map((t) => (t.id === id ? { ...t, title } : t)),
			}));
			persistDebounced();
		},

		setPtyId: (id, ptyId) => {
			set((s) => ({
				tabs: s.tabs.map((t) => (t.id === id ? { ...t, ptyId } : t)),
			}));
			// ptyIds aren't persisted, so no flush needed.
		},

		setStatus: (id, status, exitCode = null) => {
			set((s) => ({
				tabs: s.tabs.map((t) =>
					t.id === id
						? {
								...t,
								status,
								exitCode,
								wasRunning:
									status === 'running' || status === 'spawning'
										? true
										: status === 'exited'
											? false
											: t.wasRunning,
							}
						: t
				),
			}));
			persistDebounced();
		},

		attachToStudio: (tabId, paneId, artifactPath, opts) => {
			const state = get();
			const tab = state.tabs.find((t) => t.id === tabId);
			if (!tab) return { ok: true }; // gone — caller will hit stale-attachment path
			if (tab.owner.kind === 'studio' && tab.owner.paneId !== paneId && !opts?.force) {
				return {
					ok: false,
					requiresConfirm: true,
					previousPaneId: tab.owner.paneId,
				};
			}
			// Capture the displaced pane (force-reclaim case) before we overwrite
			// `owner` — its PaneView still references this tab and must be cleared
			// or the old pane keeps mounting a SingleTerminal in parallel.
			const displacedPaneId =
				tab.owner.kind === 'studio' && tab.owner.paneId !== paneId ? tab.owner.paneId : null;
			set((s) => ({
				tabs: s.tabs.map((t) =>
					t.id === tabId ? { ...t, owner: { kind: 'studio', paneId, artifactPath } } : t
				),
			}));
			persistDebounced();
			if (displacedPaneId) clearPaneAttachment(displacedPaneId);
			return { ok: true };
		},

		detachFromStudio: (tabId) => {
			// Capture the owning pane BEFORE we flip owner so we know which
			// PaneView's `attachedTerminalId` to clear in the pane store.
			const owner = get().tabs.find((t) => t.id === tabId)?.owner;
			const owningPaneId = owner?.kind === 'studio' ? owner.paneId : null;
			set((s) => ({
				tabs: s.tabs.map((t) =>
					t.id === tabId && t.owner.kind === 'studio' ? { ...t, owner: { kind: 'sidepane' } } : t
				),
			}));
			persistDebounced();
			if (owningPaneId) clearPaneAttachment(owningPaneId);
		},

		findStudioAttachment: (paneId) => {
			const state = get();
			return state.tabs.find((t) => t.owner.kind === 'studio' && t.owner.paneId === paneId) ?? null;
		},

		rehydrateFromDb: async () => {
			try {
				const persisted = await readPersisted();
				const restored: TerminalTab[] = persisted.map((p) => {
					const shouldAutoRespawn =
						p.wasRunning ?? (p.status === 'running' || p.status === 'spawning');
					return {
						...p,
						ptyId: null,
						// Restored tabs that were active before app exit restart in 'spawning' status
						status: shouldAutoRespawn ? 'spawning' : 'exited',
						wasRunning: shouldAutoRespawn,
						exitCode: p.exitCode,
						// Force-default ownership to sidepane on rehydrate. Studio
						// attachments are re-established by the Studio pane on mount
						// (saved `attachedTerminalId` in PaneView); cross-store
						// ordering with `loadPaneTree` makes restoring the saved
						// owner here fragile.
						owner: { kind: 'sidepane' },
					};
				});
				set({
					tabs: restored,
					activeId: restored[0]?.id ?? null,
					rehydrated: true,
				});
			} catch (err) {
				console.warn('[terminal/session-store] rehydrate failed', err);
				set({ rehydrated: true });
			}
		},

		persistToDb: async () => {
			await writePersisted(serialize(get().tabs));
		},
	};
});
