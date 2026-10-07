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
import { listen } from '@/lib/transport';
import { ptyTerminalList, settingsGet, type TerminalDescriptor } from '@/lib/tauri-cmd';
import { RESUME_TERMINALS_KEY } from '@/lib/shell-profiles';
import type { AgentWrapOpts } from './claude-wrap';
import { loadClaudeSettingsPath } from './claude-settings';
import { Pty } from './pty-bridge';
import { attachCapture } from './pty-output-buffer';
import { acquirePty, disposePty, getPty } from './pty-registry';
import { buildSpawnOpts } from './spawn-opts';
import { createNetworkErrnoScanner } from '@/lib/wsl-health/errno';
import { isWslTab, scansForNetworkErrno, wslTabDistro } from '@/lib/wsl-health/tabs';
import type { HookEventPayload } from './tool-call-feed';

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
	spec: { cwd: string; cmd: string[]; env?: Record<string, string>; wrap?: AgentWrapOpts };
	/** Claude session id captured from the `SessionStart` hook. Used to resume
	 *  the conversation after an app restart. */
	claudeSessionId?: string | null;
	/** True between this tab's Claude `SessionStart` and its `SessionEnd` /
	 *  PTY exit (the store-level hooks listener below). In memory only —
	 *  never persisted, so a restored tab reads not-live until a fresh
	 *  `SessionStart`. The WP-53 action runner injects only while it is true. */
	agentLive?: boolean;
	/** True while this tab shows a permission prompt the agent is waiting
	 *  on: Claude Code's own `PermissionRequest` or a held `PreToolUse`
	 *  (the store-level hooks listener below). In memory only — never
	 *  persisted. The WP-53 action runner never injects while it is true:
	 *  the inject's trailing Enter would answer the prompt. */
	permissionPending?: boolean;
	ptyId: string | null;
	mode?: 'persistent' | 'ephemeral';
	status: 'spawning' | 'running' | 'exited' | 'error';
	exitCode: number | null;
	createdAt: number;
	owner: TerminalOwner;
	wasRunning?: boolean;
}

export interface RestoreNotice {
	message: string;
	holdsSave: boolean;
	/** Where Resume saving put the unreadable list, when it did. */
	backupKey?: string;
}

interface TerminalState {
	tabs: TerminalTab[];
	activeId: string | null;
	rehydrated: boolean;
	/** Set when the last rehydrate couldn't read the saved tab list or the
	 *  resume setting. `holdsSave` means the saved list was unreadable, so
	 *  saving is paused (the next save would overwrite it with whatever is
	 *  open now) until the user chooses Resume saving. `backupKey` is set
	 *  after Resume saving copied the unreadable list to a side key. */
	restoreError: RestoreNotice | null;
	/** Clears a notice. For one that holds saving this is `resumeSaving`. */
	dismissRestoreError: () => void;
	/** D-12: copy the unreadable saved list to a timestamped side key, and
	 *  only once that worked, resume saving. Saving stays paused (with the
	 *  reason) if the copy can't be made. If the list reads fine by now (the
	 *  first failure was transient), its tabs are restored instead. */
	resumeSaving: () => Promise<void>;

	add: (spec: TerminalTab['spec'], title?: string, id?: string) => string;
	setActive: (id: string) => void;
	remove: (id: string) => void;
	rename: (id: string, title: string) => void;
	setPtyId: (id: string, ptyId: string | null, mode?: 'persistent' | 'ephemeral') => void;
	setMode: (id: string, mode: 'persistent' | 'ephemeral') => void;
	setStatus: (id: string, status: TerminalTab['status'], exitCode?: number | null) => void;
	/** A string id marks the agent live (`SessionStart`); `null` marks it
	 *  ended (`SessionEnd`, PTY exit, restart). */
	setClaudeSessionId: (id: string, sessionId: string | null) => void;
	updateCwd: (id: string, cwd: string) => void;

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
	claudeSessionId?: string | null;
	ptyId?: string | null;
	mode?: 'persistent' | 'ephemeral';
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
	return tabs.map(
		({
			id,
			title,
			spec,
			claudeSessionId,
			ptyId,
			mode,
			status,
			exitCode,
			createdAt,
			owner,
			wasRunning: prevWasRunning,
		}) => {
			const isCurrentlyRunning = status === 'running' || status === 'spawning';
			const wasRunning = isCurrentlyRunning || Boolean(prevWasRunning);
			const wrap = spec.wrap
				? { ...spec.wrap, terminalId: undefined, resumeSessionId: undefined }
				: undefined;
			// Contract G-01: Only persistent (daemon-backed) sessions retain their ptyId across reload/restart.
			// Ephemeral (in-process) sessions die on reload/restart.
			const isPersistent = mode === 'persistent';
			return {
				id,
				title,
				// Strip credential-shaped env vars before persisting (ADR-013
				// §Addendum Decision 3) — the restored tab is a durable on-disk record.
				spec: { ...spec, env: stripSecretEnv(spec.env), wrap },
				claudeSessionId,
				ptyId: isPersistent ? ptyId : null,
				mode: isPersistent ? 'persistent' : 'ephemeral',
				status: isCurrentlyRunning ? (isPersistent && ptyId ? 'running' : 'exited') : status,
				wasRunning,
				exitCode,
				createdAt,
				owner,
			};
		}
	);
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

function errText(err: unknown): string {
	return err instanceof Error ? err.message : String(err);
}

/** The saved tab list, or `error` when it exists but couldn't be read. A
 *  failed read is never reported as "no saved tabs". */
async function readPersisted(): Promise<{ tabs: SerializedTab[]; error: string | null }> {
	const db = await loadDb();
	if (db) {
		let rows: { value: string }[];
		try {
			rows = await db.select<{ value: string }[]>(
				'SELECT value FROM layout_state WHERE key = $1',
				[STORAGE_KEY]
			);
		} catch (err) {
			console.warn('[terminal/session-store] read failed', err);
			return { tabs: [], error: `couldn't read the saved terminal list (${errText(err)})` };
		}
		if (!rows || rows.length === 0) return { tabs: [], error: null };
		return parseSerialized(rows[0].value);
	}
	// localStorage fallback (no SQL plugin).
	let raw: string | null;
	try {
		raw = localStorage.getItem(STORAGE_KEY);
	} catch (err) {
		return { tabs: [], error: `couldn't read the saved terminal list (${errText(err)})` };
	}
	if (!raw) return { tabs: [], error: null };
	return parseSerialized(raw);
}

type SavedLocation = 'sql' | 'local';

/** Side key an unreadable saved list is copied to before saving resumes. */
export function unreadableBackupKey(now: number = Date.now()): string {
	return `${STORAGE_KEY}.unreadable-${now}`;
}

/** The saved list's raw text (null when there is none) and where it lives.
 *  Throws when it can't be read. */
async function readRawSaved(): Promise<{ raw: string | null; where: SavedLocation }> {
	const db = await loadDb();
	if (db) {
		const rows = await db.select<{ value: string }[]>(
			'SELECT value FROM layout_state WHERE key = $1',
			[STORAGE_KEY]
		);
		return { raw: rows && rows.length > 0 ? rows[0].value : null, where: 'sql' };
	}
	return { raw: localStorage.getItem(STORAGE_KEY), where: 'local' };
}

/** Write `value` under `key` next to the saved list. Throws on failure —
 *  unlike `writePersisted`, a backup must not silently land elsewhere. */
async function writeSideKey(where: SavedLocation, key: string, value: string): Promise<void> {
	if (where === 'sql') {
		const db = await loadDb();
		if (!db) throw new Error('the terminal database is unavailable');
		await db.execute(
			'INSERT INTO layout_state (key, value, updated_at) VALUES ($1, $2, $3) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at',
			[key, value, Date.now()]
		);
		return;
	}
	localStorage.setItem(key, value);
}

function describeLocation(where: SavedLocation): string {
	return where === 'sql' ? 'ikenga-terminal.sqlite (layout_state)' : "this window's local storage";
}

function parseSerialized(raw: string): { tabs: SerializedTab[]; error: string | null } {
	try {
		const parsed: unknown = JSON.parse(raw);
		if (Array.isArray(parsed)) return { tabs: parsed as SerializedTab[], error: null };
		return { tabs: [], error: 'the saved terminal list is not a list' };
	} catch (err) {
		return { tabs: [], error: `the saved terminal list is unreadable (${errText(err)})` };
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

/** Exported for callers that need to mint a tab id before building its argv. */
export function makeTerminalId(): string {
	return makeId();
}

/** The resume-on-start setting, or `error` when it couldn't be read (the
 *  caller then skips auto-resume, but says why). */
async function readResumeSetting(): Promise<{ resume: boolean; error: string | null }> {
	try {
		const v = await settingsGet(RESUME_TERMINALS_KEY);
		if (v == null) return { resume: false, error: null };
		return { resume: v === 'true' || v === '1', error: null };
	} catch (err) {
		return { resume: false, error: errText(err) };
	}
}

/** True while an unreadable saved list must not be overwritten. */
function saveHeld(): boolean {
	return useTerminalStore.getState().restoreError?.holdsSave === true;
}

/**
 * The one way to get a tab's PTY: attaches when the tab carries a `ptyId`,
 * otherwise spawns. Single-flight per tab id (see `acquirePty`), so the
 * rehydrate auto-resume and any number of SingleTerminal mounts/re-renders
 * share one PTY instead of each opening their own.
 */
export function openTabPty(tab: TerminalTab, opts: { forceEphemeral?: boolean } = {}): Promise<Pty> {
	const attachId = tab.ptyId;
	return acquirePty(
		tab.id,
		async () => {
			if (attachId) return Pty.attach(attachId, tab.title);
			// Prime the per-terminal `--settings` path so claude terminals wire
			// their hooks to the live bridge. Failure is non-fatal: the session
			// falls back to running without live telemetry.
			await loadClaudeSettingsPath().catch(() => {});
			// WP-2 (D-7): probe WSL's network before a WSL launch (cached 30 s).
			// Fire-and-forget — it only feeds the pane banner, never gates or
			// fails the spawn. Lazy so this store doesn't load the query layer.
			if (isWslTab(tab)) {
				const distro = wslTabDistro(tab);
				void import('@/lib/wsl-health/query')
					.then((m) => m.prelaunchWslProbe(distro))
					.catch(() => {});
			}
			const spawnOpts = buildSpawnOpts(tab, tab.id);
			// `forceEphemeral`: an in-process PTY Rust can see (a seat's terminal,
			// G-SEATS P-10), never the daemon.
			return Pty.spawn(opts.forceEphemeral ? { ...spawnOpts, forceEphemeral: true } : spawnOpts);
		},
		(pty) => {
			const store = useTerminalStore.getState();
			// WP-2 (D-7): a WSL tab printing a network errno (EAI_AGAIN, …)
			// forces a WSL health probe (debounced, once per episode). A
			// passive tee like `attachCapture` below: it renders nothing, and
			// the replay-buffer retention rule (pty-bridge.ts) is unchanged —
			// the capture is already a non-rendering subscriber. Spawns only: a
			// reattach replays old scrollback (see `scansForNetworkErrno`).
			let offErrno: (() => void) | null = null;
			if (scansForNetworkErrno(tab, { attached: Boolean(attachId) })) {
				const distro = wslTabDistro(tab);
				offErrno = pty.onData(
					createNetworkErrnoScanner(() => {
						void import('@/lib/wsl-health/query')
							.then((m) => m.reportWslNetworkErrno(distro))
							.catch(() => {});
					})
				);
			}
			pty.onExit((code) => {
				offErrno?.();
				// Drop the dead PTY from the registry so a click-to-respawn finds
				// a clean slate, and forget its resume id.
				disposePty(tab.id);
				const s = useTerminalStore.getState();
				s.setPtyId(tab.id, null);
				s.setClaudeSessionId(tab.id, null);
				if (pty.sessionLost) s.setStatus(tab.id, 'error');
				else s.setStatus(tab.id, 'exited', code);
			});
			// Tee PTY bytes into a per-session ring buffer so iyke can read the
			// visible/scrollback content without screenshotting xterm's canvas.
			attachCapture(tab.id, pty);
			store.setPtyId(tab.id, pty.id, pty.mode);
			store.setStatus(tab.id, 'running');
		}
	);
}

/** A saved tab as it comes back after a reload: reattached to a live PTY
 *  when one survived, otherwise marked to respawn or left exited. */
function toRestoredTab(
	p: SerializedTab,
	liveByTerminalId: Map<string, TerminalDescriptor> | null
): TerminalTab {
	const shouldAutoRespawn =
		p.wasRunning ?? (p.status === 'running' || p.status === 'spawning');
	const live = liveByTerminalId?.get(p.id);
	if (live && shouldAutoRespawn) {
		// The PTY survived the refresh: reattach instead of respawning.
		return {
			...p,
			ptyId: live.pty_id,
			mode: 'ephemeral',
			status: 'running',
			wasRunning: true,
			exitCode: null,
			owner: { kind: 'sidepane' },
		};
	}

	// Persistent (daemon-backed) tab that survived reload/restart:
	// Attempt reattach to the daemon session.
	if (p.mode === 'persistent' && p.ptyId && shouldAutoRespawn) {
		return {
			...p,
			ptyId: p.ptyId,
			mode: 'persistent',
			status: 'running',
			wasRunning: true,
			exitCode: null,
			owner: { kind: 'sidepane' },
		};
	}

	return {
		...p,
		ptyId: null,
		mode: p.mode ?? 'ephemeral',
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
}

/** Live PTYs by terminal id, or null when the list can't be read. */
async function liveTerminalsById(): Promise<Map<string, TerminalDescriptor> | null> {
	try {
		const live = await ptyTerminalList();
		return new Map(live.map((d) => [d.terminal_id, d]));
	} catch (err) {
		console.warn('[terminal/session-store] ptyTerminalList failed during rehydrate', err);
		return null;
	}
}

async function respawnTab(tab: TerminalTab): Promise<void> {
	if (tab.ptyId || getPty(tab.id)) return;
	try {
		await openTabPty(tab);
	} catch (err) {
		console.error('[session-store] auto-respawn failed for', tab.id, err);
		useTerminalStore.getState().setStatus(tab.id, 'error');
	}
}

export const useTerminalStore = create<TerminalState>((set, get) => {
	const persistDebounced = debounce(() => {
		if (get().restoreError?.holdsSave) return;
		void writePersisted(serialize(get().tabs));
	}, 300);

	/** Bring back a saved list that is readable after all: its tabs join the
	 *  ones opened meanwhile (same id = already open), then saving resumes. */
	const restoreReadableList = async (saved: SerializedTab[]): Promise<void> => {
		const liveByTerminalId = await liveTerminalsById();
		const openIds = new Set(get().tabs.map((t) => t.id));
		const restored = saved
			.filter((p) => !openIds.has(p.id))
			.map((p) => toRestoredTab(p, liveByTerminalId));
		set((s) => ({
			tabs: [...s.tabs, ...restored],
			activeId: s.activeId ?? restored[0]?.id ?? null,
			restoreError: null,
		}));
		persistDebounced();
		const { resume } = await readResumeSetting();
		if (resume) {
			for (const tab of restored) {
				if (tab.status === 'spawning') void respawnTab(tab);
			}
		}
	};

	return {
		tabs: [],
		activeId: null,
		rehydrated: false,
		restoreError: null,

		dismissRestoreError: () => {
			if (get().restoreError?.holdsSave) {
				void get().resumeSaving();
				return;
			}
			set({ restoreError: null });
		},

		resumeSaving: async () => {
			const notice = get().restoreError;
			if (!notice?.holdsSave) return;
			let backupKey: string | undefined;
			let where: SavedLocation | undefined;
			let readable: SerializedTab[] | null = null;
			try {
				const saved = await readRawSaved();
				where = saved.where;
				if (saved.raw) {
					const parsed = parseSerialized(saved.raw);
					if (parsed.error === null) {
						// The earlier failure was transient and the list reads fine
						// now: restore it rather than back it up and overwrite it.
						readable = parsed.tabs;
					} else {
						backupKey = unreadableBackupKey();
						await writeSideKey(saved.where, backupKey, saved.raw);
					}
				}
			} catch (err) {
				// Without a copy, resuming would destroy the list: stay paused.
				// A read that keeps failing keeps saving paused for the session;
				// that is deliberate (D-12: nothing is overwritten unbacked).
				set({
					restoreError: {
						message: `Couldn't back up the unreadable terminal list (${errText(err)}), so saving is still paused.`,
						holdsSave: true,
					},
				});
				return;
			}
			if (readable) {
				await restoreReadableList(readable);
				return;
			}
			set({
				restoreError: backupKey
					? {
							message: `Saving resumed. The unreadable terminal list was copied to "${backupKey}" in ${describeLocation(where ?? 'local')}.`,
							holdsSave: false,
							backupKey,
						}
					: null,
			});
			// Saving was paused; catch up with what is open now.
			persistDebounced();
		},

		add: (spec, title, id) => {
			const tabId = id ?? makeId();
			const tab: TerminalTab = {
				id: tabId,
				title: title ?? spec.cmd[0] ?? 'shell',
				spec,
				ptyId: null,
				status: 'spawning',
				exitCode: null,
				createdAt: Date.now(),
				owner: { kind: 'sidepane' },
			};
			set((s) => ({ tabs: [...s.tabs, tab], activeId: tabId }));
			persistDebounced();
			return tabId;
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

		setPtyId: (id, ptyId, mode) => {
			set((s) => ({
				tabs: s.tabs.map((t) => (t.id === id ? { ...t, ptyId, ...(mode ? { mode } : {}) } : t)),
			}));
			persistDebounced();
		},

		setMode: (id, mode) => {
			set((s) => ({
				tabs: s.tabs.map((t) => (t.id === id ? { ...t, mode } : t)),
			}));
			persistDebounced();
		},

		setStatus: (id, status, exitCode = null) => {
			set((s) => ({
				tabs: s.tabs.map((t) =>
					t.id === id
						? {
								...t,
								status,
								exitCode,
								// PTY exit ends the agent and any prompt; a (re)spawn
								// starts from neither until its own hooks say so.
								...(status === 'exited' || status === 'error' || status === 'spawning'
									? { agentLive: false, permissionPending: false }
									: {}),
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

		setClaudeSessionId: (id, sessionId) => {
			set((s) => ({
				tabs: s.tabs.map((t) =>
					t.id === id
						? { ...t, claudeSessionId: sessionId, agentLive: typeof sessionId === 'string' }
						: t
				),
			}));
			persistDebounced();
		},

		updateCwd: (id, cwd) => {
			set((s) => ({
				tabs: s.tabs.map((t) => (t.id === id ? { ...t, spec: { ...t.spec, cwd } } : t)),
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
				const { tabs: persisted, error: readError } = await readPersisted();
				if (readError) {
					set({
						restoreError: {
							message: `Couldn't restore your previous terminals: ${readError}. Terminal saving is paused so that list isn't overwritten. Resume saving to replace it with the terminals open now.`,
							holdsSave: true,
						},
					});
				}

				// On a webview reload the Tauri process (and its PTYs) survive.
				// Reconcile against the live PTY list before deciding to respawn.
				const liveByTerminalId = await liveTerminalsById();

				const restored = persisted.map((p) => toRestoredTab(p, liveByTerminalId));
				set({
					tabs: restored,
					activeId: restored[0]?.id ?? null,
					rehydrated: true,
				});

				// App restart: rehydrate restores previously running tabs in the
				// 'spawning' state. Respawning here instead of inside SingleTerminal
				// means unfocused panes also get a PTY immediately, not when the
				// user switches to them. Skipped when the user turns the setting off.
				const { resume, error: resumeError } = await readResumeSetting();
				if (resumeError && restored.some((t) => t.status === 'spawning') && !get().restoreError) {
					set({
						restoreError: {
							message: `Couldn't read the "resume terminals" setting (${resumeError}), so your previous terminals weren't restarted. Open one to start it.`,
							holdsSave: false,
						},
					});
				}
				if (resume) {
					for (const tab of restored) {
						if (tab.status === 'spawning') {
							void respawnTab(tab);
						}
					}
				}
			} catch (err) {
				console.warn('[terminal/session-store] rehydrate failed', err);
				set({
					rehydrated: true,
					restoreError: get().restoreError ?? {
						message: `Couldn't restore your previous terminals: ${errText(err)}.`,
						holdsSave: false,
					},
				});
			}
		},

		persistToDb: async () => {
			if (get().restoreError?.holdsSave) return;
			await writePersisted(serialize(get().tabs));
		},
	};
});

// --- agent liveness + pending permission prompts (hooks bus) --------------

/** The hook payload fields this listener reads; a held `PreToolUse` also
 *  carries `request_id` + `held` (`iyke/hooks.rs`). */
export type AgentHookPayload = HookEventPayload & { request_id?: string; held?: boolean };

/** One prompt still up in a tab, matched against the tool call that
 *  finishes it the way `notifications/producers.rs::PendingPrompt` does:
 *  by `tool_use_id` when both sides carry one, else tool name + input. */
interface PendingPrompt {
	toolUseId?: string;
	fingerprint: string;
}

/** Pending prompts per tab id. Only meaningful while the tab's
 *  `permissionPending` is true: `setStatus` clears the flag on PTY exit /
 *  respawn, and the next prompt then starts a fresh list. */
const pendingPrompts = new Map<string, PendingPrompt[]>();

/** Bound per tab, as `MAX_PROMPTS_PER_TERMINAL` in `producers.rs`. */
const MAX_PROMPTS_PER_TAB = 32;

function pendingPrompt(p: AgentHookPayload): PendingPrompt {
	return {
		...(p.tool_use_id ? { toolUseId: p.tool_use_id } : {}),
		fingerprint: `${p.tool_name ?? ''}:${JSON.stringify(p.tool_input ?? null)}`,
	};
}

function promptMatches(a: PendingPrompt, b: PendingPrompt): boolean {
	return a.toolUseId && b.toolUseId ? a.toolUseId === b.toolUseId : a.fingerprint === b.fingerprint;
}

function setPermissionPending(tabId: string, pending: boolean): void {
	if (!pending) pendingPrompts.delete(tabId);
	useTerminalStore.setState((s) => ({
		tabs: s.tabs.map((t) => (t.id === tabId ? { ...t, permissionPending: pending } : t)),
	}));
}

/** Hook events that report one tool call as done (`producers.rs::finishes_terminal_tool`). */
const FINISHES_TOOL = new Set(['PostToolUse', 'PostToolUseFailure']);
/** Hook events Claude cannot reach while a prompt is up — they end every
 *  prompt in the tab (`producers.rs::ends_terminal_permissions`). */
const ENDS_PROMPTS = new Set(['Stop', 'SessionEnd', 'UserPromptSubmit']);

/**
 * Records one Claude Code hook event against the tab it came from
 * (`ikenga_terminal_id`). Store-level, so a tab that no pane currently
 * mounts is tracked too.
 *
 * - Liveness: `SessionStart` stores the session id and marks the agent live
 *   — only while the tab's PTY is `running` (a late `SessionStart` after
 *   the PTY exited is ignored); `SessionEnd` clears both. PTY exit and
 *   respawn clear them via `setStatus`. There is no separate
 *   agent-process-exit signal: a `claude` killed without a `SessionEnd`
 *   stays live until the PTY exits.
 * - Permission prompts (mirrors Rust's `terminal_prompts`, `iyke/hooks.rs`):
 *   `PermissionRequest` (Claude's own in-terminal Yes/No — sent with no
 *   `request_id`, so the Companion queue never sees it) and a held
 *   `PreToolUse` set `permissionPending`. `PostToolUse` /
 *   `PostToolUseFailure` of the very tool call a prompt asked about
 *   removes that prompt (a parallel call finishing does not count);
 *   `Stop` / `SessionEnd` / `UserPromptSubmit` clear them all; PTY exit and
 *   respawn clear them via `setStatus`.
 */
export function applyAgentHook(p: AgentHookPayload | null | undefined): void {
	const tabId = p?.ikenga_terminal_id;
	if (!p || !tabId) return;
	const store = useTerminalStore.getState();
	const tab = store.tabs.find((t) => t.id === tabId);
	if (!tab) return;
	const event = p.hook_event_name ?? '';

	if (event === 'SessionStart' && p.session_id) {
		if (tab.status === 'running') store.setClaudeSessionId(tabId, p.session_id);
	} else if (event === 'SessionEnd') {
		// The claude session ended; the PTY may keep going (the wrap's
		// fallback shell) but there is no agent and nothing to resume.
		store.setClaudeSessionId(tabId, null);
	}

	if (event === 'PermissionRequest' || (event === 'PreToolUse' && (p.held === true || Boolean(p.request_id)))) {
		const list = tab.permissionPending ? (pendingPrompts.get(tabId) ?? []) : [];
		if (list.length >= MAX_PROMPTS_PER_TAB) list.shift();
		list.push(pendingPrompt(p));
		pendingPrompts.set(tabId, list);
		setPermissionPending(tabId, true);
	} else if (FINISHES_TOOL.has(event)) {
		if (!tab.permissionPending) return;
		const done = pendingPrompt(p);
		const list = pendingPrompts.get(tabId) ?? [];
		const at = list.findIndex((q) => promptMatches(q, done));
		if (at >= 0) list.splice(at, 1);
		if (list.length === 0) setPermissionPending(tabId, false);
	} else if (ENDS_PROMPTS.has(event)) {
		if (tab.permissionPending) setPermissionPending(tabId, false);
	}
}

let agentHookListener: Promise<unknown> | null = null;

/** Installs the one store-level `hooks://event` listener. Idempotent. */
export function installAgentHookListener(): void {
	if (agentHookListener) return;
	agentHookListener = listen<AgentHookPayload>('hooks://event', (event) =>
		applyAgentHook(event.payload)
	).catch(() => {
		agentHookListener = null;
	});
}

if (typeof window !== 'undefined') installAgentHookListener();

// Immediate close-flush on beforeunload (issue #133): ensures pending tab state is written on app exit
if (typeof window !== 'undefined') {
	window.addEventListener('beforeunload', () => {
		if (saveHeld()) return;
		const tabs = useTerminalStore.getState().tabs;
		void writePersisted(serialize(tabs));
	});
}
