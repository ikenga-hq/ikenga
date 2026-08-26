// Typed wrappers around Tauri commands. This file is the cross-team contract
// between rust-eng (implements the commands in src-tauri/) and frontend-shell
// (consumes them from React). Keep in sync with src-tauri/src/commands/.
//
// Phase 1 surface area: pty (already implemented in spike), fs (read / list /
// watch), secrets (Stronghold), db (SQLite), viewer (axum localhost), and
// stubs for agent / render that arrive in later phases. Stubs return
// `unimplemented!()` from Rust for phase 1 — the wrappers are typed today so
// later phases just fill in the Rust side.

import type { WindowDescriptor } from '@ikenga/contract';
import { getTransport, isRemoteWebSession, isTauri, type RpcTransport } from './transport';
import { attachRemotePty } from './transport/pty-socket';

export { isTauri, isRemoteWebSession, getTransport, type RpcTransport };
export type UnlistenFn = () => void;

function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	return getTransport().invoke<T>(cmd, args);
}

function listen<T>(
	event: string,
	handler: (event: { event: string; payload: T }) => void
): Promise<UnlistenFn> {
	return getTransport().listen<T>(event, handler);
}

// ─── PTY ──────────────────────────────────────────────────────────────────────

export interface PtySpawnOpts {
	terminalId?: string;
	title?: string;
	cwd: string;
	cmd: string[];
	env?: Record<string, string>;
	rows?: number;
	cols?: number;
}

export async function ptySpawn(opts: PtySpawnOpts): Promise<string> {
	return invoke<string>('pty_spawn', {
		terminalId: opts.terminalId ?? null,
		title: opts.title ?? null,
		cwd: opts.cwd,
		cmd: opts.cmd,
		env: opts.env ?? null,
		rows: opts.rows ?? 24,
		cols: opts.cols ?? 80,
	});
}

export async function ptyWrite(id: string, data: string): Promise<void> {
	return invoke('pty_write', { id, data });
}

export async function ptyResize(id: string, rows: number, cols: number): Promise<void> {
	return invoke('pty_resize', { id, rows, cols });
}

export async function ptyKill(id: string): Promise<void> {
	return invoke('pty_kill', { id });
}

/** Foreground process snapshot for a single PTY — what the user is actually
 *  running in the terminal right now (e.g. `claude`, `bash`, `vim`). Returns
 *  `null` when the PTY is gone or the platform doesn't yet surface it
 *  (macOS / Windows in v0). Cached for 1s per PTY in the Rust side. */
export interface ForegroundProcess {
	pid: number;
	/** Executable basename — e.g. `"claude"`, `"bash"`. */
	name: string;
	/** Full argv, null-byte-stripped. Empty when the kernel returned nothing. */
	args: string[];
}

export async function ptyForeground(id: string): Promise<ForegroundProcess | null> {
	return invoke<ForegroundProcess | null>('pty_foreground', { id });
}

/** Foreground snapshot across every live PTY. Keyed by PTY id; only PTYs
 *  whose foreground lookup succeeded appear in the result. The routing
 *  dispatcher uses this to pick the active claude session for pin delivery. */
export async function ptyForegroundSnapshot(): Promise<Record<string, ForegroundProcess>> {
	return invoke<Record<string, ForegroundProcess>>('pty_foreground_snapshot');
}

/** One live terminal as the Rust core sees it — the same shape
 *  `GET /iyke/terminal/list` serves to agents. Mirrors `TerminalDescriptor` in
 *  `src-tauri/src/pty/mod.rs`, which serializes with its Rust field names (no
 *  `rename_all`), so these stay snake_case.
 *
 *  Only the fields the shell actually consumes are typed here; the Rust struct
 *  carries more (lease/audit offsets) that agents read over the bridge. */
export interface TerminalDescriptor {
	/** Session-store tab id (`TerminalTab.id`). Falls back to the pty id when
	 *  the spawn didn't declare one, so it is never empty. */
	terminal_id: string;
	pty_id: string;
	/** Title the spawner chose — e.g. `claude`, or a Studio preset name.
	 *  Defaults to the joined argv (`"bash -l"`) when the spawn passed none. */
	title: string;
	/** Agent-assigned name, set via `POST /iyke/terminal/label`. Unique among
	 *  live terminals; null until an agent claims one. */
	label: string | null;
	cwd: string;
	argv: string[];
	status: 'running' | 'exited';
	pid: number | null;
	/** What the terminal is running RIGHT NOW (`claude`, `vim`, `bash`).
	 *  Null on platforms that can't observe the foreground process group. */
	foreground_command: ForegroundProcess | null;
	/** Agent currently holding the write lease, if any. */
	owner_agent_id: string | null;
}

/** Every live terminal descriptor, oldest first. Backs terminal tab titles —
 *  see `src/terminal/terminal-title.ts`. */
export async function ptyTerminalList(): Promise<TerminalDescriptor[]> {
	return invoke<TerminalDescriptor[]>('pty_terminal_list');
}

export interface ShellProfile {
	id: string;
	label: string;
	icon: string;
	cmd: string[];
	isDefault: boolean;
	kind: 'powershell' | 'pwsh' | 'wsl' | 'bash' | 'cmd' | 'zsh' | string;
	distro?: string | null;
}

/** Detect available shell profiles on the host machine. */
export async function terminalDetectShells(): Promise<ShellProfile[]> {
	return invoke<ShellProfile[]>('terminal_detect_shells');
}

/**
 * Subscribe to PTY byte stream + exit. Backend emits each data chunk as
 * `"<endOffset>:<base64>"` — base64 because Tauri serializes payloads as JSON
 * and Uint8Array doesn't survive cleanly, and the offset prefix is the
 * cumulative byte count the chunk ends at (so `endOffset - bytes.length` is its
 * absolute start). `onData` receives that offset; consumers that don't dedup
 * against a scrollback snapshot can ignore it.
 */
export async function ptyListen(
	id: string,
	onData: (bytes: Uint8Array, endOffset: number) => void,
	onExit: (code: number | null) => void
): Promise<UnlistenFn> {
	if (isRemoteWebSession()) {
		const transport = getTransport();
		if (transport.openPtySocket) {
			return attachRemotePty(transport.openPtySocket.bind(transport), id, onData, onExit);
		}
	}

	const dataUnlisten = await listen<string>(`pty://${id}`, (e) => {
		const raw = e.payload;
		const sep = raw.indexOf(':');
		const endOffset = sep >= 0 ? Number(raw.slice(0, sep)) : 0;
		const b64 = sep >= 0 ? raw.slice(sep + 1) : raw;
		const bin = atob(b64);
		const arr = new Uint8Array(bin.length);
		for (let i = 0; i < bin.length; i++) arr[i] = bin.charCodeAt(i);
		onData(arr, endOffset);
	});
	const exitUnlisten = await listen<number | null>(`pty://${id}/exit`, (e) => {
		onExit(e.payload);
	});
	return () => {
		dataUnlisten();
		exitUnlisten();
	};
}

/** Trailing scrollback of a PTY, replayed by a window that attaches after the
 *  PTY has already produced output (a popped-out terminal), plus the token that
 *  releases the gate `ptyAttachBegin` installed. `endOffset` is the cumulative
 *  byte count `data` ends at — and, because Rust gates the stream from the same
 *  instant it takes this snapshot, it is also the exact offset the first live
 *  chunk after `ptyAttachArm` starts at. Snapshot and live stream tile without
 *  overlap, so there is nothing to dedup. `null` once the PTY has exited +
 *  been reaped. */
export interface PtyAttachSnapshot {
	data: Uint8Array;
	endOffset: number;
	token: number;
}

/** Step 1 of the atomic attach handshake — snapshot + gate. MUST be followed by
 *  `ptyAttachArm` once the caller's `ptyListen` subscription is registered;
 *  until then the PTY emits nothing to anyone. Rust runs a 2s watchdog so a
 *  caller that dies in between can't stall the terminal. */
export async function ptyAttachBegin(id: string): Promise<PtyAttachSnapshot | null> {
	const res = await invoke<{ data: string; endOffset: number; token: number } | null>(
		'pty_attach_begin',
		{ id }
	);
	if (!res) return null;
	const bin = atob(res.data);
	const arr = new Uint8Array(bin.length);
	for (let i = 0; i < bin.length; i++) arr[i] = bin.charCodeAt(i);
	return { data: arr, endOffset: res.endOffset, token: res.token };
}

/** Step 2 of the atomic attach handshake — release the gate. Everything emitted
 *  during the handshake arrives as the first live chunk. `false` means the gate
 *  was already released (watchdog / superseded); that is a no-op, not an error. */
export async function ptyAttachArm(id: string, token: number): Promise<boolean> {
	return invoke<boolean>('pty_attach_arm', { id, token });
}

// ─── FS ───────────────────────────────────────────────────────────────────────
//
// Main-window plugin-fs is intentionally empty-scoped — `capabilities/default.json`
// lists the fs:* perm identifiers with empty allow arrays so no static main-window
// fs grant exists. All app fs goes through the Rust commands below (fsRead, fsList,
// etc.), which enforce a user-configured allowlist via `fs_roots::FsRoots` (Settings
// → Storage → File roots). Per-pkg fs is granted dynamically by the perms registry's
// `add_capability` at install time, independent of this file.

export interface FileEntry {
	path: string;
	name: string;
	isDir: boolean;
	size: number;
	modifiedMs: number;
}

export interface FileChange {
	kind: 'create' | 'modify' | 'remove' | 'rename';
	path: string;
}

export interface FileReadResult {
	bytes: number[];
	mime: string;
}

export async function fsRead(path: string): Promise<FileReadResult> {
	return invoke('fs_read', { path });
}

/** Cheap MIME lookup that doesn't read file contents. Used by the artifact
 * viewer's auto-router as a fallback when JS extension lookup misses. */
export async function fsMime(path: string): Promise<string> {
	return invoke('fs_mime', { path });
}

/** Cheap existence check — returns true if `path` resolves to a regular
 * file (not a directory). Allowlisted same as the other fs commands. */
export async function fsExists(path: string): Promise<boolean> {
	return invoke('fs_exists', { path });
}

/** Cheap kind discriminator — returns `'file' | 'dir' | 'missing'` for an
 *  allowlisted path. Backs the unified artifact-studio route resolver
 *  (folder → grid density, file → loupe density). `'missing'` covers
 *  both not-found and allowlist-rejected. */
export async function fsKind(path: string): Promise<'file' | 'dir' | 'missing'> {
	return invoke('fs_kind', { path });
}

export async function fsWrite(path: string, bytes: Uint8Array): Promise<void> {
	return invoke('fs_write', { path, bytes: Array.from(bytes) });
}

/** UTF-8 encode + write. Mirror of the TextDecoder read path in renderers. */
export async function fsWriteText(path: string, text: string): Promise<void> {
	return fsWrite(path, new TextEncoder().encode(text));
}

/** Recursive mkdir. Idempotent — succeeds if the directory already exists. */
export async function fsMkdir(path: string): Promise<void> {
	return invoke('fs_mkdir', { path });
}

export async function fsList(dir: string, glob?: string): Promise<FileEntry[]> {
	return invoke('fs_list', { dir, glob: glob ?? null });
}

/** Move a file or directory to the OS trash (reversible). */
export async function fsTrash(path: string): Promise<void> {
	return invoke('fs_trash', { path });
}

/** Rename in place to a new basename. Returns the resolved destination path. */
export async function fsRename(from: string, toName: string): Promise<string> {
	return invoke('fs_rename', { from, toName });
}

export interface FsSearchResult {
	matches: string[];
	truncated: boolean;
}

/** Recursive case-insensitive basename search rooted at `root`. Honors the
 *  same dot-file / ignored-dir rules the files-explorer sorter uses, so hits
 *  match what the user would see after manually expanding every folder.
 *  Capped server-side (default 500); `truncated` flags an early stop. */
export async function fsSearch(
	root: string,
	query: string,
	showHidden: boolean,
	showIgnored: boolean,
	limit?: number
): Promise<FsSearchResult> {
	return invoke('fs_search', { root, query, showHidden, showIgnored, limit: limit ?? null });
}

/** Returns a watcher id; events emit on `fs://{watcherId}`. */
export async function fsWatch(path: string): Promise<string> {
	return invoke('fs_watch', { path });
}

// ─── FS allowlist (user-configurable) ────────────────────────────────────────
//
// The allowlist is owned by Rust (`src-tauri/src/fs_roots.rs`, persisted to
// `app_data_dir/fs_roots.json`). Every mutation returns the canonical list so
// the frontend can sync from the response rather than maintaining a parallel
// store. Default roots: `~/royalti-co`, `~/.claude/projects`, `~/.company`.

export async function fsRootsList(): Promise<string[]> {
	return invoke('fs_roots_list');
}

export async function fsRootsAdd(path: string): Promise<string[]> {
	return invoke('fs_roots_add', { path });
}

export async function fsRootsRemove(path: string): Promise<string[]> {
	return invoke('fs_roots_remove', { path });
}

export async function fsRootsReset(): Promise<string[]> {
	return invoke('fs_roots_reset');
}

/** OS-level username fallback (`$USER`/`%USERNAME%`, or `"unknown"`). Used
 *  to populate `hostContext.operator` when the onboarding display name
 *  (`useShellStore().userName`) is empty. */
export async function osUsername(): Promise<string> {
	return invoke('os_username');
}

export async function fsUnwatch(watcherId: string): Promise<void> {
	return invoke('fs_unwatch', { watcherId });
}

export async function fsListenWatch(
	watcherId: string,
	onChange: (change: FileChange) => void
): Promise<UnlistenFn> {
	return listen<FileChange>(`fs://${watcherId}`, (e) => onChange(e.payload));
}

// ─── Settings KV (durable mirror for Zustand-persisted prefs) ───────────────
//
// Backed by SQLite `settings_kv` table (migration 0013). Zustand stores
// continue to write to localStorage for instant-paint hydration; this layer
// is the authoritative copy that survives "Clear local data" and can later
// feed a cross-device sync. See `useShellStore.hydrateSettingsFromRust`.
// Values are JSON strings — type discipline is enforced by callers.

export async function settingsGet(key: string): Promise<string | null> {
	return invoke('settings_get', { key });
}

export async function settingsSet(key: string, value: string): Promise<void> {
	return invoke('settings_set', { key, value });
}

export async function settingsGetAll(): Promise<Record<string, string>> {
	return invoke('settings_get_all');
}

export async function settingsClearAll(): Promise<void> {
	return invoke('settings_clear_all');
}

// ─── Secrets (Stronghold) ─────────────────────────────────────────────────────

export async function secretsGet(key: string): Promise<string | null> {
	return invoke('secrets_get', { key });
}

export async function secretsSet(key: string, value: string): Promise<void> {
	return invoke('secrets_set', { key, value });
}

export async function secretsDelete(key: string): Promise<void> {
	return invoke('secrets_delete', { key });
}

export async function secretsListKeys(): Promise<string[]> {
	return invoke('secrets_list_keys');
}

// ─── Phase 7 — scoped secrets ─────────────────────────────────────────────

/** Vault scope discriminator matching Rust's `commands::secrets::Scope`.
 *  Serde tags this enum with `kind` and snake-cases the variants, so the
 *  wire shape is `{ kind: "workspace" } | { kind: "project", id } | ...`. */
export type VaultScope =
	| { kind: 'workspace' }
	| { kind: 'project'; id: string }
	| { kind: 'pkg'; id: string };

export async function secretsGetScoped(scope: VaultScope, key: string): Promise<string | null> {
	return invoke('secrets_get_scoped', { scope, key });
}

export async function secretsSetScoped(
	scope: VaultScope,
	key: string,
	value: string
): Promise<void> {
	return invoke('secrets_set_scoped', { scope, key, value });
}

export async function secretsDeleteScoped(scope: VaultScope, key: string): Promise<void> {
	return invoke('secrets_delete_scoped', { scope, key });
}

export async function secretsListKeysScoped(scope: VaultScope): Promise<string[]> {
	return invoke('secrets_list_keys_scoped', { scope });
}

export type VaultStatus = {
	available: boolean;
	keychainBackend: string;
	error: string | null;
};

export async function secretsVaultStatus(): Promise<VaultStatus> {
	// Rust returns snake_case `keychain_backend`; normalize.
	const raw = await invoke<{ available: boolean; keychain_backend: string; error: string | null }>(
		'secrets_vault_status'
	);
	return {
		available: raw.available,
		keychainBackend: raw.keychain_backend,
		error: raw.error,
	};
}

// ─── Supabase project config (URL + anon key, stored as a non-secret JSON
// manifest at app_data_dir/supabase.json — see commands/supabase_config.rs).

export type SupabaseConfig = {
	url: string;
	anonKey: string;
	serviceRoleKey?: string | null;
};

export async function supabaseConfigGet(): Promise<SupabaseConfig | null> {
	const raw = await invoke<{
		url: string;
		anon_key: string;
		service_role_key?: string | null;
	} | null>('supabase_config_get');
	return raw
		? { url: raw.url, anonKey: raw.anon_key, serviceRoleKey: raw.service_role_key ?? null }
		: null;
}

export async function supabaseConfigSet(
	url: string,
	anonKey: string,
	serviceRoleKey?: string | null
): Promise<void> {
	return invoke('supabase_config_set', {
		url,
		anonKey,
		serviceRoleKey: serviceRoleKey ?? null,
	});
}

export async function supabaseConfigClear(): Promise<void> {
	return invoke('supabase_config_clear');
}

// ─── DB (SQLite — layout state, recents) ─────────────────────────────────
//
// Thin wrapper over tauri-plugin-sql. The plugin can be used directly via
// `import Database from '@tauri-apps/plugin-sql'` — these helpers exist for
// callers who don't want to manage the connection.

export type SqlValue = string | number | boolean | null | number[];

export async function dbQuery<T = unknown>(sql: string, params: SqlValue[] = []): Promise<T[]> {
	return invoke('db_query', { sql, params });
}

export async function dbExec(sql: string, params: SqlValue[] = []): Promise<void> {
	return invoke('db_exec', { sql, params });
}

// ─── agent-ops host bridge (WP-09 / G-TRIGGER) ────────────────────────────────
//
// Privileged hops the agent-ops iframe pkg can't make: trigger an out-of-schedule
// run on the always-on cron daemon (localhost endpoint, 0600 lock token),
// flip a job's enabled flag in the project-scoped config, and read the daemon's
// config + state files. The Rust commands always resolve with a structured
// `{ ok, ... }` payload (typed `code` on failure) — see contract `host-verbs.ts`.

/** `Value` returned by the Rust commands (see `AgentOps*Result` in @ikenga/contract). */
export async function agentOpsRunNow(jobId: string): Promise<unknown> {
	return invoke('agent_ops_run_now', { jobId });
}

export async function agentOpsSetEnabled(jobId: string, enabled: boolean): Promise<unknown> {
	return invoke('agent_ops_set_enabled', { jobId, enabled });
}

export async function agentOpsListJobs(): Promise<unknown> {
	return invoke('agent_ops_list_jobs', {});
}

export async function agentOpsUpsertJob(job: unknown): Promise<unknown> {
	return invoke('agent_ops_upsert_job', { job });
}

export async function agentOpsDeleteJob(jobId: string): Promise<unknown> {
	return invoke('agent_ops_delete_job', { jobId });
}

/** Mirror of `AgentOpsTailRunResult` in @ikenga/contract. Pure FS read on the
 *  shell's own loop (never touches the daemon) — reads the per-job marker +
 *  per-run tail file by byte-range for the pkg's Live-output view. Script-mode
 *  only; agent jobs return an empty chunk with `mode:'agent'`. */
export type AgentOpsTailRunResult =
	| {
			ok: true;
			running: boolean;
			status: 'running' | 'done' | null;
			startedAtMs: number | null;
			mode: 'agent' | 'script' | null;
			chunk: string;
			nextOffset: number;
			eof: boolean;
	  }
	| { ok: false; code: string; status: number | null; error: string };

/** Read live (or last-completed) run output for a job by byte-range. Poll with
 *  `offset:0` first, then feed the returned `nextOffset` back each poll while
 *  `running` is true. After completion the final chunk is still returned for
 *  scrollback. */
export async function agentOpsTailRun(
	jobId: string,
	offset?: number
): Promise<AgentOpsTailRunResult> {
	return invoke('agent_ops_tail_run', { jobId, offset });
}

// ─── Viewer (shared axum server, same-origin via Vite proxy / localhost-plugin)

export interface ViewerHandle {
	/** Shell-origin-relative URL prefix, e.g. `/__viewer/<token>/`. Append the
	 *  file path to get a full iframe `src`. Resolved against
	 *  `window.location.origin` so it works under both Vite (dev) and
	 *  localhost-plugin (prod). */
	url: string;
	/** Pass to `viewerStop` to release the mount. */
	token: string;
}

export async function viewerServe(rootDir: string): Promise<ViewerHandle> {
	return invoke('viewer_serve', { rootDir });
}

export async function viewerStop(token: string): Promise<void> {
	return invoke('viewer_stop', { token });
}

/** Bound port of the shared viewer server. `null` if start failed. */
export async function viewerPort(): Promise<number | null> {
	return invoke('viewer_port');
}

// ─── Claude config browser (/claude route) ────────────────────────────────────
//
// Read-only scan of `.claude/{agents,skills,commands}` and `.claude/settings*.json`
// across user-managed project roots + the personal `~/.claude/` dir. Backed by
// `commands/claude_config.rs` which uses `serde_yaml` to parse frontmatter and
// reuses `FsWatchManager` for live updates.

export type ClaudeConfigScope = 'project' | 'personal';

export interface ClaudeFrontmatter {
	[key: string]: unknown;
}

/** Symlink / central-store metadata shared by every scanned primitive.
 *  Mirrors the Rust `LinkMeta` enrichment (`commands/claude_config.rs`).
 *  Additive + back-compatible: file-based primitives (agents, commands,
 *  skills) populate these from an lstat; JSON-fragment primitives (hooks,
 *  MCP servers) always report `isSymlink: false`, `linkTarget: null`,
 *  `inStore: false` because they're toggled by JSON merge, not the symlink
 *  farm. WP-02/03 consume these to drive enable/disable + store-catalog UI. */
export interface ClaudeLinkMeta {
	/** Whether the on-disk primitive (file or dir) is a symlink. */
	isSymlink: boolean;
	/** Resolved symlink target path, or `null` when not a symlink. */
	linkTarget: string | null;
	/** Whether the resolved target lives inside the Ngwa central store
	 *  (`<app_data_dir>/store/`). */
	inStore: boolean;
	/** WP-03 (Ọba registry): whether a symlink resolves to an existing target.
	 *  `false` = dangling (orphaned); otherwise the link resolves to a valid
	 *  master and reads as *linked* regardless of `inStore`. Absent ⇒ treat as
	 *  resolving (only an explicit `false` means dangling). */
	targetExists?: boolean;
	// ── Ngwa Phase-2 cross-system entry extension (frozen contract, WP-19) ──
	//
	// Three additive fields every scan-result entry gains so the FE can group
	// primitives by engine. The Rust scanner (WP-17) fills these for the real
	// scan; the multi-engine dev mock below fills them for canned data. Reuses
	// the `EngineId` / `ConfigFormat` / `KindStatus` enums mirrored from the
	// G-ADAPTER descriptor (see `engineLayout()` below). Scope is the existing
	// `scope` field on each entry — NOT a new field.
	//
	// Optional-with-default: WP-17's Rust struct always emits all three, but the
	// fields are optional here so pre-existing Claude-only literals (graph tests,
	// ngwa-surface) still type without a backfill. Consumers (WP-20) treat an
	// absent `system` as `"claude"` and an absent `status` as `"active"`.
	/** Which AI-coding engine owns this primitive. Absent ⇒ `"claude"`. */
	system?: EngineId;
	/** On-disk serialization format of the primitive. */
	format?: ConfigFormat;
	/** Live vs. deprecated (e.g. Codex commands → skills). Absent ⇒ `"active"`. */
	status?: KindStatus;
}

export interface ClaudeAgent extends ClaudeLinkMeta {
	name: string;
	scope: ClaudeConfigScope;
	projectRoot: string | null;
	path: string;
	modifiedMs: number;
	description: string | null;
	model: string | null;
	frontmatter: ClaudeFrontmatter;
	body: string;
	overriddenBy: string | null;
}

export interface ClaudeSupportingFile {
	name: string;
	path: string;
	size: number;
}

export interface ClaudeSkill extends ClaudeLinkMeta {
	name: string;
	scope: ClaudeConfigScope;
	projectRoot: string | null;
	path: string;
	dirPath: string;
	modifiedMs: number;
	description: string | null;
	frontmatter: ClaudeFrontmatter;
	body: string;
	supportingFiles: ClaudeSupportingFile[];
	overriddenBy: string | null;
}

export interface ClaudeCommand extends ClaudeLinkMeta {
	name: string;
	scope: ClaudeConfigScope;
	projectRoot: string | null;
	path: string;
	modifiedMs: number;
	description: string | null;
	model: string | null;
	argumentHint: string | null;
	frontmatter: ClaudeFrontmatter;
	body: string;
	overriddenBy: string | null;
}

export interface ClaudeMcp extends ClaudeLinkMeta {
	name: string;
	scope: ClaudeConfigScope;
	projectRoot: string | null;
	path: string;
	transport: 'stdio' | 'http' | 'sse' | 'unknown' | string;
	command: string | null;
	args: string[];
	envKeys: string[];
	url: string | null;
	headerKeys: string[];
	raw: unknown;
}

export interface ClaudeHook extends ClaudeLinkMeta {
	event: string;
	type: string;
	name: string;
	scope: ClaudeConfigScope;
	projectRoot: string | null;
	settingsPath: string;
	commandPath: string | null;
	commandRaw: string | null;
	raw: unknown;
}

export interface ClaudeConfigScanError {
	path: string;
	message: string;
}

export interface ClaudeConfig {
	agents: ClaudeAgent[];
	skills: ClaudeSkill[];
	commands: ClaudeCommand[];
	hooks: ClaudeHook[];
	mcps: ClaudeMcp[];
	errors: ClaudeConfigScanError[];
}

// ─── Ngwa Phase-2 multi-engine scan mock (WP-19) ──────────────────────────────
//
// WP-17/18 land the real Rust scanner that fills `system` / `format` / `status`
// on every scan entry across Claude + Gemini + Codex. Until they merge,
// `claude_config_load` only knows about Claude, so this dev-flag mock returns a
// small canned multi-engine dataset so WP-20 can build the engine-grouped facet
// ahead of the backend.
//
// CUTOVER DONE (Phase 2 v2a, commit 65ed44d): WP-17/18 ship the live multi-engine
// scan (real Claude + Gemini + Codex), so the mock now defaults OFF — the dev shell
// shows the real config, not canned data. Flip to `true` to exercise the FE against
// the canned multi-engine dataset below without a live `.gemini`/`.codex` present.
const NGWA_SCAN_MOCK = false;

/** Canned multi-engine scan the mock returns. Spans Claude + Gemini + Codex
 *  across kinds, and deliberately exercises the WP-20 facet edge cases:
 *    • one Codex `status: "deprecated"` command (Codex commands → skills),
 *    • one Gemini `format: "toml"` command,
 *    • one same-named agent (`reviewer`) under both Gemini (md-yaml) and Codex
 *      (toml) so the engine-grouped facet shows the cross-engine collision.
 *  Claude entries keep `system: "claude"` so the default-engine grouping reads
 *  naturally. */
const ngwaScanMockConfig: ClaudeConfig = {
	agents: [
		{
			name: 'rex',
			scope: 'personal',
			projectRoot: null,
			path: '/home/dev/.claude/agents/rex.md',
			modifiedMs: 1_716_500_000_000,
			description: 'Release-engineering agent.',
			model: null,
			frontmatter: {},
			body: '',
			overriddenBy: null,
			isSymlink: false,
			linkTarget: null,
			inStore: false,
			system: 'claude',
			format: 'md-yaml',
			status: 'active',
		},
		{
			// same-named agent under Gemini (md-yaml) …
			name: 'reviewer',
			scope: 'project',
			projectRoot: '/home/dev/projects/ikenga',
			path: '/home/dev/projects/ikenga/.gemini/agents/reviewer.md',
			modifiedMs: 1_716_480_000_000,
			description: 'Code-review agent (Gemini).',
			model: null,
			frontmatter: {},
			body: '',
			overriddenBy: null,
			isSymlink: false,
			linkTarget: null,
			inStore: false,
			system: 'gemini',
			format: 'md-yaml',
			status: 'active',
		},
		{
			// … and the SAME name under Codex (toml) — cross-engine collision.
			name: 'reviewer',
			scope: 'project',
			projectRoot: '/home/dev/projects/ikenga',
			path: '/home/dev/projects/ikenga/.codex/agents/reviewer.toml',
			modifiedMs: 1_716_470_000_000,
			description: 'Code-review agent (Codex).',
			model: null,
			frontmatter: {},
			body: '',
			overriddenBy: null,
			isSymlink: false,
			linkTarget: null,
			inStore: false,
			system: 'codex',
			format: 'toml',
			status: 'active',
		},
	],
	skills: [
		{
			name: 'huashu-design',
			scope: 'personal',
			projectRoot: null,
			path: '/home/dev/.claude/skills/huashu-design/SKILL.md',
			dirPath: '/home/dev/.claude/skills/huashu-design',
			modifiedMs: 1_716_460_000_000,
			description: 'HTML hi-fi prototyping + design advisor.',
			frontmatter: {},
			body: '',
			supportingFiles: [],
			overriddenBy: null,
			isSymlink: false,
			linkTarget: null,
			inStore: false,
			system: 'claude',
			format: 'md-yaml',
			status: 'active',
		},
		{
			name: 'gemini-extensions',
			scope: 'project',
			projectRoot: '/home/dev/projects/ikenga',
			path: '/home/dev/projects/ikenga/.gemini/skills/gemini-extensions/SKILL.md',
			dirPath: '/home/dev/projects/ikenga/.gemini/skills/gemini-extensions',
			modifiedMs: 1_716_450_000_000,
			description: 'Gemini-side helper skill.',
			frontmatter: {},
			body: '',
			supportingFiles: [],
			overriddenBy: null,
			isSymlink: false,
			linkTarget: null,
			inStore: false,
			system: 'gemini',
			format: 'md-yaml',
			status: 'active',
		},
	],
	commands: [
		{
			name: 'blog-pipeline',
			scope: 'personal',
			projectRoot: null,
			path: '/home/dev/.claude/commands/blog-pipeline.md',
			modifiedMs: 1_716_440_000_000,
			description: 'Full blog creation workflow.',
			model: null,
			argumentHint: null,
			frontmatter: {},
			body: '',
			overriddenBy: null,
			isSymlink: false,
			linkTarget: null,
			inStore: false,
			system: 'claude',
			format: 'md-yaml',
			status: 'active',
		},
		{
			// Gemini command in TOML format.
			name: 'deploy',
			scope: 'project',
			projectRoot: '/home/dev/projects/ikenga',
			path: '/home/dev/projects/ikenga/.gemini/commands/deploy.toml',
			modifiedMs: 1_716_430_000_000,
			description: 'Deploy pipeline (Gemini, TOML).',
			model: null,
			argumentHint: null,
			frontmatter: {},
			body: '',
			overriddenBy: null,
			isSymlink: false,
			linkTarget: null,
			inStore: false,
			system: 'gemini',
			format: 'toml',
			status: 'active',
		},
		{
			// Codex command — deprecated (Codex steers commands → skills).
			name: 'summarize',
			scope: 'personal',
			projectRoot: null,
			path: '/home/dev/.codex/prompts/summarize.toml',
			modifiedMs: 1_716_420_000_000,
			description: 'Legacy Codex prompt; superseded by a skill.',
			model: null,
			argumentHint: null,
			frontmatter: {},
			body: '',
			overriddenBy: null,
			isSymlink: false,
			linkTarget: null,
			inStore: false,
			system: 'codex',
			format: 'toml',
			status: 'deprecated',
		},
	],
	hooks: [
		{
			event: 'PostToolUse',
			type: 'command',
			name: 'format-on-save',
			scope: 'personal',
			projectRoot: null,
			settingsPath: '/home/dev/.claude/settings.json',
			commandPath: null,
			commandRaw: 'biome format --write $CLAUDE_FILE',
			raw: {},
			isSymlink: false,
			linkTarget: null,
			inStore: false,
			system: 'claude',
			format: 'json-embedded',
			status: 'active',
		},
	],
	mcps: [
		{
			name: 'royalti-cms',
			scope: 'personal',
			projectRoot: null,
			path: '/home/dev/.claude/settings.json',
			transport: 'http',
			command: null,
			args: [],
			envKeys: [],
			url: 'https://cms.royalti.io/mcp',
			headerKeys: ['Authorization'],
			raw: {},
			isSymlink: false,
			linkTarget: null,
			inStore: false,
			system: 'claude',
			format: 'json-embedded',
			status: 'active',
		},
		{
			// Codex MCP server declared in TOML config.
			name: 'codex-fs',
			scope: 'project',
			projectRoot: '/home/dev/projects/ikenga',
			path: '/home/dev/projects/ikenga/.codex/config.toml',
			transport: 'stdio',
			command: 'codex-fs-server',
			args: ['--root', '.'],
			envKeys: [],
			url: null,
			headerKeys: [],
			raw: {},
			isSymlink: false,
			linkTarget: null,
			inStore: false,
			system: 'codex',
			format: 'toml',
			status: 'active',
		},
	],
	errors: [],
};

/** Resolve the canned multi-engine config through a microtask so consumers see
 *  real Promise scheduling, matching the async shape of a live `invoke`. */
async function ngwaScanMockResolve(): Promise<ClaudeConfig> {
	await Promise.resolve();
	// Return a shallow structural clone so a consumer mutating the result can't
	// poison the canned dataset across calls.
	return {
		agents: [...ngwaScanMockConfig.agents],
		skills: [...ngwaScanMockConfig.skills],
		commands: [...ngwaScanMockConfig.commands],
		hooks: [...ngwaScanMockConfig.hooks],
		mcps: [...ngwaScanMockConfig.mcps],
		errors: [...ngwaScanMockConfig.errors],
	};
}

export async function claudeConfigLoad(projectRoots: string[]): Promise<ClaudeConfig> {
	if (NGWA_SCAN_MOCK) {
		return ngwaScanMockResolve();
	}
	return invoke<ClaudeConfig>('claude_config_load', { projectRoots });
}

export async function claudeConfigWatch(projectRoots: string[]): Promise<string[]> {
	return invoke<string[]>('claude_config_watch', { projectRoots });
}

export async function claudeConfigUnwatch(watcherIds: string[]): Promise<void> {
	return invoke('claude_config_unwatch', { watcherIds });
}

export async function claudeConfigReadFile(path: string): Promise<string> {
	return invoke<string>('claude_config_read_file', { path });
}

/** Subscribe to any change under any watched .claude/ dir. The Rust watcher
 *  emits per-watcher events (`fs://{id}`); this helper subscribes to all of
 *  them and debounces invalidation so the consumer can simply call
 *  `queryClient.invalidateQueries({queryKey:['claude-config']})`.
 */
export async function claudeConfigListen(
	watcherIds: string[],
	onChange: () => void
): Promise<UnlistenFn> {
	const unlisteners = await Promise.all(
		watcherIds.map((id) => listen<unknown>(`fs://${id}`, () => onChange()))
	);
	return () => {
		for (const u of unlisteners) u();
	};
}

// ─── Ngwa Phase-2 cross-system — G-ADAPTER engine layout descriptor ──────────
//
// Mirrors `src-tauri/src/commands/engine_layout.rs`. A per-engine data
// descriptor recording, for each AI-coding engine and each primitive kind,
// WHERE its config lives, in WHAT format, and HOW it is stored. This is the
// frozen `G-ADAPTER` contract the Phase-2 scanner (WP-17/18) and this bridge
// (WP-19) consume. v2a is read-only — fetch + display only.
//
// Keep these unions in lockstep with the Rust serde enums (kebab-case rename).

export type EngineId = 'claude' | 'gemini' | 'codex';

export type ScopeTier = 'user' | 'project';

export type PrimitiveKind = 'skill' | 'agent' | 'command' | 'hook' | 'mcp';

export type ConfigFormat = 'md-yaml' | 'toml' | 'json-embedded';

export type Mechanism = 'symlink-dir' | 'file' | 'settings-key';

export type KindStatus = 'active' | 'deprecated';

export interface ScopeDef {
	/** Stable scope id, referenced by `KindLayout.scopes`. */
	id: string;
	/** Human label for the sidebar facet. */
	label: string;
	/** User-global vs. per-project. */
	tier: ScopeTier;
	/** Path template / source note the scope root resolves from. */
	rootSource: string;
}

export interface KindLayout {
	/** `true` if this engine has this primitive kind at all. */
	exists: boolean;
	/** Live vs. deprecated (Codex commands → skills). */
	status: KindStatus;
	/** Which scope ids (a subset of `EngineLayout.scopes`) this kind supports. */
	scopes: string[];
	/** On-disk serialization format. */
	format: ConfigFormat;
	/** How it's stored / toggled. */
	mechanism: Mechanism;
	/** Path template. `{root}`/`{user_root}` → scope root; `{settings_file}` /
	 *  `{mcp_file}` → backing file; `{name}` → primitive name. SettingsKey
	 *  templates carry a `#path.to.key` fragment. */
	location: string;
	/** Strict-key validation on the backing settings file (Gemini
	 *  settings.json = true). v2b write-guard hook; v2a ignores it. */
	strictKeys: boolean;
}

export interface EngineLayout {
	engine: EngineId;
	/** Display name (e.g. "Claude Code"). */
	display: string;
	/** Short badge for interleaved rows (`CL` / `GM` / `CX`). */
	badge: string;
	/** Per-engine tint token name (Dusk Wood theme var, no leading `--`). */
	tint: string;
	scopes: ScopeDef[];
	/** Per-kind layout, keyed by `PrimitiveKind`. */
	kinds: Partial<Record<PrimitiveKind, KindLayout>>;
}

/** Fetch the frozen G-ADAPTER layout descriptor for all engines (read-only). */
export async function engineLayout(): Promise<EngineLayout[]> {
	return invoke<EngineLayout[]>('engine_layout');
}

// ─── Claude config — 4-tier layered discovery (Phase 4) ──────────────────────
//
// New surface for the Claude Config Browser UI. Returns *all* sources for each
// asset name (skill / agent / command / hook / mcp) across the four tiers so
// the UI can render conflicts and let the user pin a preferred provider.
//
// The legacy `claudeConfigLoad` helpers above stay around for the existing
// `/claude` route until that page migrates.

export type ClaudeAssetTier = 'personal' | 'workspace_pkg' | 'project' | 'project_pkg';

export type ClaudeAssetKind = 'skill' | 'agent' | 'command' | 'hook' | 'mcp' | 'bundle';

export interface ClaudeAssetSource {
	tier: ClaudeAssetTier;
	/** pkg id, "personal", or "project:<id>". */
	provider: string;
	path: string;
	name: string;
	kind: ClaudeAssetKind;
}

export interface ClaudeAssetTree {
	skills: Record<string, ClaudeAssetSource[]>;
	agents: Record<string, ClaudeAssetSource[]>;
	commands: Record<string, ClaudeAssetSource[]>;
	hooks: Record<string, ClaudeAssetSource[]>;
	mcps: Record<string, ClaudeAssetSource[]>;
}

export interface ClaudeAssetPin {
	scope: string;
	asset_kind: ClaudeAssetKind;
	asset_name: string;
	preferred_tier: ClaudeAssetTier;
	preferred_source: string | null;
	updated_at: number;
}

/** Run the 4-tier discovery for `projectId` (defaults to the active project). */
export async function claudeAssetsDiscover(projectId?: string | null): Promise<ClaudeAssetTree> {
	return invoke<ClaudeAssetTree>('claude_assets_discover', {
		projectId: projectId ?? null,
	});
}

/** Pin an asset name to a tier (optionally a specific pkg-id provider). */
export async function claudeAssetPin(
	scope: string,
	assetKind: ClaudeAssetKind,
	assetName: string,
	preferredTier: ClaudeAssetTier,
	preferredSource?: string | null
): Promise<void> {
	return invoke('claude_asset_pin', {
		scope,
		assetKind,
		assetName,
		preferredTier,
		preferredSource: preferredSource ?? null,
	});
}

export async function claudeAssetUnpin(
	scope: string,
	assetKind: ClaudeAssetKind,
	assetName: string
): Promise<void> {
	return invoke('claude_asset_unpin', { scope, assetKind, assetName });
}

export async function claudeAssetListPins(scope: string): Promise<ClaudeAssetPin[]> {
	return invoke<ClaudeAssetPin[]>('claude_asset_list_pins', { scope });
}

// ─── Ngwa store layer — G-CONTRACT (frozen signatures) ────────────────────────
//
// FROZEN INTERFACE CONTRACT for the Ngwa (Claude config manager) Phase-1 build.
// Published by WP-01 (scanner metadata); implemented by:
//   • WP-02 — Rust store + symlink farm (claude_store_list / import / enable /
//             disable, the file-based mutations), lib.rs registration.
//   • WP-03 — Rust JSON merge engine (powers enable/disable for hooks + MCPs,
//             and the copy/move/remove paths for JSON-fragment primitives).
//   • WP-05 — fills the FE wrapper bodies below + the TanStack mutation hooks
//             (mirrors the secrets.ts invalidate pattern: invalidate
//             `['claude-config']` / `['claude-store']` on success).
//
// The `invoke()` bodies here are the canonical wire shape. They are written out
// (not stubbed) so the contract is unambiguous — but the matching Rust commands
// land in WP-02/03, so calling these before then rejects at the Tauri boundary
// ("command not found"). WP-05 owns wiring them into hooks + UI.
//
// Conventions matched: snake_case command names + camelCase invoke args (as in
// the scanner + secrets surfaces); the kind/scope/tier vocabularies reuse the
// pin-layer types above so the whole Claude surface speaks one dialect.

/** Which primitive kind a store operation targets. Same vocabulary as the pin
 *  layer (`ClaudeAssetKind`). File-based kinds (`skill | agent | command`) flow
 *  through the symlink farm; JSON-fragment kinds (`hook | mcp`) flow through the
 *  merge engine — but every command below accepts the full set so callers don't
 *  branch on kind. */
export type ClaudeStoreKind = ClaudeAssetKind;

/** Target location for an enable/copy/move. `workspace` writes into the
 *  workspace-level `.claude/`; `project:<id>` into that project's `.claude/`.
 *  Mirrors the pin-layer scope grammar validated by `validate_pin_scope`. */
export type ClaudeStoreScope = 'workspace' | `project:${string}`;

/** A single catalog entry in the Ngwa central store (Ọba). One row per
 *  canonical primitive the store owns; `enabledIn` lists the scopes that
 *  currently symlink/merge it so the UI can render per-scope state badges. */
/** One forward-dependency edge (`requires[]` element, ADR-015 §3 / WP-11).
 *  Mirrors the Rust `RequiresEntry` in `pkg/manifest.rs` + `RequiresEntrySchema`
 *  in `@ikenga/contract`. A separate graph from a skill's `depends_on`. */
export interface RequiresEntry {
	/** Primitive kind: skill | agent | command | hook | mcp. */
	kind: string;
	/** Primitive name (e.g. `skill-core`, `@ikenga/studio-beat-detect`). */
	name: string;
	/** Optional fetch source; absent → resolver looks it up in the registry/catalog. */
	source?: 'git' | 'npx' | 'catalog' | 'local';
	/** Optional git tag/branch or version pin. */
	ref?: string;
}

export interface ClaudeStoreEntry {
	kind: ClaudeStoreKind;
	/** Catalog name (the primitive's name, e.g. skill/agent/command name). */
	name: string;
	/** Absolute path of the canonical copy inside the store. For file-based
	 *  kinds this is the symlink target; for JSON kinds it's the stored
	 *  fragment file. */
	storePath: string;
	/** Optional human description lifted from frontmatter / the fragment. */
	description: string | null;
	/** mtime of the stored canonical copy (epoch ms). */
	modifiedMs: number;
	/** Scopes this entry is currently enabled in (symlinked or merged). */
	enabledIn: ClaudeStoreScope[];

	/** Forward dependencies (ADR-015 §3 / WP-11) — the compiled `requires` list.
	 *  A SIBLING of provenance: provenance is origin, `requires` is what this
	 *  primitive needs (the Ọba resolver installs the closure). Absent → no deps.
	 *  Mirrors the `requires` field on the Rust `ClaudeStoreEntry` + the pkg
	 *  manifest `RequiresEntry` (lockstep). */
	requires?: RequiresEntry[];

	/** Member skills shipped by a `bundle` (WP-18). A bundle is a directory
	 *  primitive (`store/bundles/<name>/`) holding N member skill subdirs; this
	 *  lists the members the install placed. A SIBLING of `requires`. Absent → no
	 *  members (every non-bundle entry). Mirrors the `members` field on the Rust
	 *  `ClaudeStoreEntry` (`#[serde(default)]`). Populated by the bundle installer
	 *  (WP-19); WP-18 only freezes the field. */
	members?: string[];

	// ─── Registry provenance (G-SCHEMA · Ọba registry) ───────────────────────
	// Mirrors the flattened `RegistryProvenance` on the Rust `ClaudeStoreEntry`.
	// The backend ALWAYS populates these on real responses (serde synthesizes a
	// `local` entry for pre-registry data); they are optional here only for the
	// vestigial `ngwaMockStore` + forward-compat. Dependents are NOT here — they
	// are computed live by the scanner, never stored (see drafts/registry-schema.md).

	/** Origin of the canonical master. Absent → treat as `'local'`. */
	source?: 'local' | 'git' | 'npx' | 'catalog';
	/** git remote URL | npm spec | catalog id; null for local. */
	url?: string | null;
	/** Requested git tag/branch (resolves to `version`); null otherwise. */
	ref?: string | null;
	/** Resolved git commit SHA | npm version; null for local. */
	version?: string | null;
	/** Absolute path to the real master — may be in-vault OR external/in-place. */
	canonicalPath?: string;
	/** true = vault master (shell owns lifecycle, deletable); false = external
	 *  master kept in place, never hard-deleted by the safe-delete guard. */
	managed?: boolean;
	installedAt?: string | null;
	updatedAt?: string | null;
	/** Phase 3: true = discovered through the recommended Ọba catalog. Orthogonal
	 *  to `source` (the resolved fetch mechanism). Absent → false. */
	fromCatalog?: boolean;
	/** Phase 3: true = the shell may auto-update this master on the catalog
	 *  surface mount. Catalog installs default this on; manual/local off. Absent
	 *  → false. */
	autoUpdate?: boolean;
}

/** Result of a symlink-farm or merge mutation. `path` is the on-disk location
 *  the mutation produced (the new symlink, the merged settings file, or the
 *  removed path); `linkTarget` is populated for symlink creates. */
export interface ClaudeStoreMutation {
	kind: ClaudeStoreKind;
	name: string;
	scope: ClaudeStoreScope;
	path: string;
	linkTarget: string | null;
}

// ─── Ngwa store mock layer (WP-05) ─────────────────────────────────────────
//
// The Rust commands for the frozen `claude_store_*` / `claude_primitive_*`
// surface land in WP-02/03. Until they merge, calling these wrappers rejects
// at the Tauri boundary ("command not found"). To let WP-07 build the full
// Ngwa UI ahead of the backend, the wrappers route through a dev-flag mock
// that returns typed canned data instead of calling `invoke`.
//
// CUTOVER (single line for the orchestrator): set `NGWA_STORE_MOCK = false`
// below once WP-02/03 have registered the Rust commands. Nothing else changes
// — every wrapper falls straight through to its frozen `invoke(...)` body.
//
// The flag defaults to ON in dev builds and OFF in production builds; the
// explicit `false` cutover removes the mock in every build.
// Cut over to live by the orchestrator at Phase-1 backend integration (WP-02/03/04
// landed + wired). Wrappers now fall through to their frozen `invoke(...)` bodies.
const NGWA_STORE_MOCK: boolean = false;

/** In-memory catalog the mock resolves against. Mock mutations mutate it so
 *  the UI sees enable/disable/copy/move/remove reflect immediately during dev.
 *  Spans every kind + the enabled / disabled / local / orphaned matrix via the
 *  `enabledIn` scope sets. */
const ngwaMockStore: ClaudeStoreEntry[] = [
	{
		kind: 'skill',
		name: 'huashu-design',
		storePath: '/home/dev/.local/share/ikenga/store/skills/huashu-design',
		description: 'HTML hi-fi prototyping + design advisor + expert review.',
		modifiedMs: 1_716_500_000_000,
		// store-backed symlink enabled in two scopes.
		enabledIn: ['workspace', 'project:ikenga'],
	},
	{
		kind: 'skill',
		name: 'release-status',
		storePath: '/home/dev/.local/share/ikenga/store/skills/release-status',
		description: 'Scan child repos for unreleased commits + registry drift.',
		modifiedMs: 1_716_400_000_000,
		// enabled in a single scope.
		enabledIn: ['workspace'],
	},
	{
		kind: 'agent',
		name: 'rex',
		storePath: '/home/dev/.local/share/ikenga/store/agents/rex.md',
		description: 'Release-engineering agent.',
		modifiedMs: 1_716_300_000_000,
		// in the catalog but not enabled anywhere — orphaned / available.
		enabledIn: [],
	},
	{
		kind: 'command',
		name: 'blog-pipeline',
		storePath: '/home/dev/.local/share/ikenga/store/commands/blog-pipeline.md',
		description: 'Full blog creation workflow.',
		modifiedMs: 1_716_200_000_000,
		enabledIn: ['project:website'],
	},
	{
		kind: 'hook',
		name: 'format-on-save',
		storePath: '/home/dev/.local/share/ikenga/store/hooks/format-on-save.json',
		description: 'PostToolUse hook that runs biome on edited files.',
		modifiedMs: 1_716_100_000_000,
		// JSON-fragment kind enabled via settings merge in workspace.
		enabledIn: ['workspace'],
	},
	{
		kind: 'mcp',
		name: 'royalti-cms',
		storePath: '/home/dev/.local/share/ikenga/store/mcps/royalti-cms.json',
		description: 'Royalti CMS MCP server (http transport).',
		modifiedMs: 1_716_050_000_000,
		// JSON-fragment kind not enabled anywhere yet.
		enabledIn: [],
	},
];

/** Resolve through a microtask so consumers see real Promise scheduling,
 *  matching the async shape of a live `invoke`. */
async function ngwaMockResolve<T>(value: T): Promise<T> {
	await Promise.resolve();
	return value;
}

function ngwaMockFind(kind: ClaudeStoreKind, name: string): ClaudeStoreEntry | undefined {
	return ngwaMockStore.find((e) => e.kind === kind && e.name === name);
}

function ngwaMockMutation(
	kind: ClaudeStoreKind,
	name: string,
	scope: ClaudeStoreScope
): ClaudeStoreMutation {
	const entry = ngwaMockFind(kind, name);
	const fileBased = kind === 'skill' || kind === 'agent' || kind === 'command';
	const store = entry?.storePath ?? `/home/dev/.local/share/ikenga/store/${kind}s/${name}`;
	const scopeRoot =
		scope === 'workspace'
			? '/home/dev/workspace'
			: `/home/dev/projects/${scope.slice('project:'.length)}`;
	return {
		kind,
		name,
		scope,
		path: fileBased
			? `${scopeRoot}/.claude/${kind}s/${name}`
			: `${scopeRoot}/.claude/settings.json`,
		linkTarget: fileBased ? store : null,
	};
}

/**
 * List the catalog of canonical primitives in the central store (Ọba).
 * Optionally filter by kind. WP-02 owns the Rust body.
 *
 * G-CONTRACT: implemented by WP-05 (FE wrapper) against WP-02 (Rust).
 */
export async function claudeStoreList(kind?: ClaudeStoreKind | null): Promise<ClaudeStoreEntry[]> {
	if (NGWA_STORE_MOCK) {
		return ngwaMockResolve(
			kind ? ngwaMockStore.filter((e) => e.kind === kind) : [...ngwaMockStore]
		);
	}
	return invoke<ClaudeStoreEntry[]>('claude_store_list', { kind: kind ?? null });
}

/**
 * Import an existing on-disk primitive into the central store, taking a copy
 * as the new canonical source. `sourcePath` is the current primitive path (the
 * `path` of a scanned entry — for skills pass the skill `dirPath`). Returns the
 * resulting catalog entry. Does not change the original in place; pair with
 * `claudePrimitiveEnable` to swap the original for a store-backed symlink.
 *
 * G-CONTRACT: implemented by WP-05 (FE wrapper) against WP-02 (Rust).
 */
export async function claudeStoreImport(
	kind: ClaudeStoreKind,
	name: string,
	sourcePath: string
): Promise<ClaudeStoreEntry> {
	if (NGWA_STORE_MOCK) {
		const existing = ngwaMockFind(kind, name);
		if (existing) return ngwaMockResolve(existing);
		const entry: ClaudeStoreEntry = {
			kind,
			name,
			storePath: `/home/dev/.local/share/ikenga/store/${kind}s/${name}`,
			description: `Imported from ${sourcePath}`,
			modifiedMs: Date.now(),
			enabledIn: [],
		};
		ngwaMockStore.push(entry);
		return ngwaMockResolve(entry);
	}
	return invoke<ClaudeStoreEntry>('claude_store_import', { kind, name, sourcePath });
}

/**
 * Enable a store catalog entry in a target scope. File-based kinds create a
 * symlink in `<scope>/.claude/<kind>s/`; JSON kinds (hook/mcp) merge the stored
 * fragment into that scope's settings JSON. Idempotent — re-enabling an already
 * enabled entry is a no-op that returns the existing mutation.
 *
 * G-CONTRACT: implemented by WP-05 (FE wrapper) against WP-02/03 (Rust).
 */
export async function claudePrimitiveEnable(
	kind: ClaudeStoreKind,
	name: string,
	scope: ClaudeStoreScope
): Promise<ClaudeStoreMutation> {
	if (NGWA_STORE_MOCK) {
		const entry = ngwaMockFind(kind, name);
		if (entry && !entry.enabledIn.includes(scope)) entry.enabledIn = [...entry.enabledIn, scope];
		return ngwaMockResolve(ngwaMockMutation(kind, name, scope));
	}
	return invoke<ClaudeStoreMutation>('claude_primitive_enable', { kind, name, scope });
}

/**
 * Disable a store catalog entry in a target scope — the inverse of
 * `claudePrimitiveEnable`. Drops the symlink (file-based) or unmerges the
 * fragment from the scope's settings JSON (hook/mcp). The canonical store copy
 * is untouched. Idempotent.
 *
 * G-CONTRACT: implemented by WP-05 (FE wrapper) against WP-02/03 (Rust).
 */
export async function claudePrimitiveDisable(
	kind: ClaudeStoreKind,
	name: string,
	scope: ClaudeStoreScope
): Promise<void> {
	if (NGWA_STORE_MOCK) {
		const entry = ngwaMockFind(kind, name);
		if (entry) entry.enabledIn = entry.enabledIn.filter((s) => s !== scope);
		return ngwaMockResolve(undefined);
	}
	return invoke('claude_primitive_disable', { kind, name, scope });
}

/**
 * Copy a primitive from one scope to another, leaving the source in place.
 * File-based kinds copy the resolved file/dir into `<toScope>/.claude/`; JSON
 * kinds merge the source fragment into the destination settings JSON. Use
 * `claudePrimitiveMove` to relocate instead of duplicate.
 *
 * G-CONTRACT: implemented by WP-05 (FE wrapper) against WP-02/03 (Rust).
 */
export async function claudePrimitiveCopy(
	kind: ClaudeStoreKind,
	name: string,
	fromScope: ClaudeStoreScope,
	toScope: ClaudeStoreScope
): Promise<ClaudeStoreMutation> {
	if (NGWA_STORE_MOCK) {
		const entry = ngwaMockFind(kind, name);
		if (entry && !entry.enabledIn.includes(toScope))
			entry.enabledIn = [...entry.enabledIn, toScope];
		return ngwaMockResolve(ngwaMockMutation(kind, name, toScope));
	}
	return invoke<ClaudeStoreMutation>('claude_primitive_copy', {
		kind,
		name,
		fromScope,
		toScope,
	});
}

/**
 * Move a primitive from one scope to another (copy-then-remove-source, atomic
 * where the platform allows). Same scope semantics as `claudePrimitiveCopy`.
 *
 * G-CONTRACT: implemented by WP-05 (FE wrapper) against WP-02/03 (Rust).
 */
export async function claudePrimitiveMove(
	kind: ClaudeStoreKind,
	name: string,
	fromScope: ClaudeStoreScope,
	toScope: ClaudeStoreScope
): Promise<ClaudeStoreMutation> {
	if (NGWA_STORE_MOCK) {
		const entry = ngwaMockFind(kind, name);
		if (entry) {
			const next = entry.enabledIn.filter((s) => s !== fromScope);
			if (!next.includes(toScope)) next.push(toScope);
			entry.enabledIn = next;
		}
		return ngwaMockResolve(ngwaMockMutation(kind, name, toScope));
	}
	return invoke<ClaudeStoreMutation>('claude_primitive_move', {
		kind,
		name,
		fromScope,
		toScope,
	});
}

/**
 * Remove a primitive from a single scope's `.claude/` (delete the file/dir or
 * symlink for file-based kinds; unmerge the fragment for hook/mcp). This is a
 * scope-local delete — it does NOT remove the canonical copy from the store
 * (use a future store-delete for that). On a store-backed symlink this only
 * drops the link, identical to `claudePrimitiveDisable`; on a real (non-link)
 * primitive it deletes the actual file.
 *
 * G-CONTRACT: implemented by WP-05 (FE wrapper) against WP-02/03 (Rust).
 */
export async function claudePrimitiveRemove(
	kind: ClaudeStoreKind,
	name: string,
	scope: ClaudeStoreScope
): Promise<void> {
	if (NGWA_STORE_MOCK) {
		const entry = ngwaMockFind(kind, name);
		if (entry) entry.enabledIn = entry.enabledIn.filter((s) => s !== scope);
		return ngwaMockResolve(undefined);
	}
	return invoke('claude_primitive_remove', { kind, name, scope });
}

// ─── Ngwa v2b write engine — G-WRITE (frozen signatures, WP-22) ───────────────
//
// FROZEN per-engine write-command contract for the Phase-2 (v2b) cross-system
// WRITE layer. Published by WP-22 (the settings-embedded JSON + TOML merge
// engine + Gemini strict-key guard) for WP-23 (file-kind writes), WP-24
// (cross-engine transcode copy/move), WP-25 (adapter de-dup), and WP-26 (FE
// write affordances / D-09) to build against.
//
// What WP-22 itself ships (Rust): the engine-aware merge dispatch
// (`merge::{enable,disable}_{hook,mcp}_for(EngineId, scope, …)`) routing JSON
// vs TOML by the frozen `EngineLayout`, plus the typed `StoreError` (below).
// The corresponding Tauri *commands* — the per-engine `claude_primitive_*`
// surface threaded with an `engine` arg — are registered by WP-23/26; these
// signatures freeze their wire shape now so the FE + sibling WPs can compile
// against a stable contract before the handlers land.
//
// Vocabulary reuse: `EngineId` (claude | gemini | codex) and `ConfigFormat`
// are the G-ADAPTER types mirrored above; `ClaudeStoreScope` / `ClaudeStoreKind`
// / `ClaudeStoreMutation` are the G-CONTRACT store types. The engine arg
// *defaults* to `'claude'` so the existing Phase-1 call sites (which omit it)
// keep their exact behaviour — Claude stays the default engine.

/** Which on-disk settings file a hook write targets, for the JSON engines
 *  (Claude/Gemini). `shared` → `settings.json`; `local` → `settings.local.json`.
 *  Ignored for Codex (single `hooks.json` / inline `config.toml`). Mirrors the
 *  Rust `merge::HookFile`. */
export type NgwaHookFile = 'shared' | 'local';

/** Typed failure modes of the v2b settings-embedded write engine, mirrored from
 *  the Rust `StoreError` (`#[serde(tag = "kind")]`). A rejected write surfaces
 *  as one of these so the FE (D-09) can branch — most importantly
 *  `strictKeyRejected`, which is a refusal *before* any disk write (never a
 *  write-and-fail), so a strict Gemini `settings.json` is never corrupted. */
export type NgwaStoreError =
	/** A strict (`additionalProperties:false`) settings file — Gemini's
	 *  `settings.json` — would reject `key`; refused before write. */
	| { kind: 'strictKeyRejected'; engine: EngineId; key: string }
	/** The backing parent (`mcpServers` / `hooks` / `mcp_servers`) is a
	 *  non-object/table scalar; refusing to clobber it. */
	| { kind: 'nonTableParent'; path: string; key: string }
	/** Target file failed to parse as its declared format. */
	| { kind: 'parse'; path: string; message: string }
	/** A block value has no representation in the target format (e.g. JSON
	 *  `null` → TOML); a typed error, never a silent drop. */
	| { kind: 'unrepresentableValue'; path: string; message: string }
	/** Filesystem error (read / write / rename / mkdir). */
	| { kind: 'io'; path: string; message: string }
	/** Scope grammar / resolution error, or an unsupported (engine, kind). */
	| { kind: 'unsupported'; message: string }
	/** A cross-engine copy whose direction has no transcoder — the blocked
	 *  TOML→MD reverse (Codex agent → Claude/Gemini, Gemini command → Claude;
	 *  see `06`). Returned *before* any disk write, so the dest is never
	 *  partially created; the D-09 drawer greys these destinations. */
	| { kind: 'transcodeUnsupported'; from: string; to: string; reason: string };

/**
 * Enable (insert/overwrite) a settings-embedded primitive (`hook` | `mcp`) in a
 * scope for a specific engine. Routes JSON vs TOML by the frozen `EngineLayout`
 * (Claude/Gemini → JSON `settings.json` / `.mcp.json` / `~/.claude.json`; Codex
 * → TOML `~/.codex/config.toml`). For Gemini the strict-key guard runs first —
 * a rejected write throws an `NgwaStoreError` of kind `strictKeyRejected`
 * before touching disk. `hookFile` selects shared vs local for hooks on the
 * JSON engines (ignored for `mcp` and for Codex).
 *
 * G-WRITE: command handler registered by WP-23/26; signature frozen by WP-22.
 * `engine` defaults to `'claude'` so Phase-1 callers are unaffected.
 */
export async function claudePrimitiveEnableFor(
	engine: EngineId,
	kind: ClaudeStoreKind,
	name: string,
	scope: ClaudeStoreScope,
	hookFile: NgwaHookFile = 'shared'
): Promise<ClaudeStoreMutation> {
	return invoke<ClaudeStoreMutation>('claude_primitive_enable_for', {
		engine,
		kind,
		name,
		scope,
		hookFile,
	});
}

/**
 * Disable (remove) a settings-embedded primitive (`hook` | `mcp`) from a scope
 * for a specific engine — the inverse of {@link claudePrimitiveEnableFor}.
 * Same JSON/TOML routing; removing the last block restores the file to its
 * pre-enable byte-identical state (the empty parent is dropped). A missing
 * file / key is a no-op.
 *
 * G-WRITE: command handler registered by WP-23/26; signature frozen by WP-22.
 */
export async function claudePrimitiveDisableFor(
	engine: EngineId,
	kind: ClaudeStoreKind,
	name: string,
	scope: ClaudeStoreScope,
	hookFile: NgwaHookFile = 'shared'
): Promise<void> {
	return invoke('claude_primitive_disable_for', { engine, kind, name, scope, hookFile });
}

// ─── Ngwa v2b cross-engine transcode copy — D-09 batch (WP-24 / WP-26) ────────
//
// The multi-destination checklist drawer (D-09, WP-26) copies a single source
// primitive into N (engine, scope) destinations in one batch. Each destination
// is either a verbatim same-format copy (md→md) or a forward transcode
// (md→toml, via the existing `transcoder.rs` entry points); reverse (toml→md)
// destinations are blocked at the UI and never reach this call. The
// directionality contract is `plans/cockpit/06-cross-engine-transcode.md`.
//
// ░░ MOCK SEAM — WP-24 NOT BUILT YET ░░
// WP-24 lands the real `claude_primitive_copy_batch` Tauri command (cross-engine
// forward transcode copy/move, after WP-22 engine + WP-23 file writes). Until it
// merges, `ngwaCrossEngineCopy` routes through `NGWA_TRANSCODE_MOCK` below, which
// returns a canned per-row batch result matching the frozen wire shape — so the
// D-09 drawer + its mutation hook are fully exercised ahead of the backend.
//
// CUTOVER (single line for the orchestrator / WP-24 finalization): set
// `NGWA_TRANSCODE_MOCK = false`. The wrapper then falls straight through to its
// `invoke('claude_primitive_copy_batch', …)` body — the request + result shapes
// here ARE the frozen contract WP-24 implements against, so nothing else changes.

/** How a single destination relates to the source's format: a verbatim copy
 *  (same format, e.g. md→md), a forward transcode (md→toml), or a blocked
 *  reverse (toml→md — no reverse transcoder exists, see `06`). The drawer never
 *  submits `blocked` rows; it's here so the result/preview vocabulary is shared. */
export type NgwaTranscodeMode = 'same' | 'transcode' | 'blocked';

/** One requested destination in a cross-engine batch copy: a target engine +
 *  scope. `mode` is the resolved transcode relationship (drives the per-row
 *  cue + which transcoder entry point the backend calls). `move` flips the
 *  per-destination semantics from copy to copy-then-remove-source. */
export interface NgwaCopyDestination {
	engine: EngineId;
	scope: ClaudeStoreScope;
	mode: NgwaTranscodeMode;
}

/** Per-row outcome of a batch copy — one entry per requested destination, in
 *  request order. `ok` rows carry the produced mutation; failed rows carry a
 *  typed `NgwaStoreError` so the drawer can render partial-failure inline. This
 *  is the per-row batch-result shape D-09 renders against. */
export type NgwaCopyRowResult =
	| {
			engine: EngineId;
			scope: ClaudeStoreScope;
			mode: NgwaTranscodeMode;
			ok: true;
			mutation: ClaudeStoreMutation;
	  }
	| {
			engine: EngineId;
			scope: ClaudeStoreScope;
			mode: NgwaTranscodeMode;
			ok: false;
			error: NgwaStoreError;
	  };

export interface NgwaCopyBatchResult {
	/** One result per requested destination, in request order. */
	rows: NgwaCopyRowResult[];
}

// ░░ CUTOVER DONE ░░ — WP-24 landed `claude_primitive_copy_batch`; mock off.
const NGWA_TRANSCODE_MOCK = false;

/** Canned batch result for the mock seam: every destination "succeeds" with a
 *  synthesized mutation EXCEPT a deliberately-failing one (the first `transcode`
 *  destination targeting a `project:` scope) so the drawer's partial-failure
 *  per-row rendering is exercised in dev. Mirrors the real wire shape exactly. */
async function ngwaCrossEngineCopyMock(
	fromEngine: EngineId,
	kind: ClaudeStoreKind,
	name: string,
	fromScope: ClaudeStoreScope,
	destinations: NgwaCopyDestination[],
	move: boolean
): Promise<NgwaCopyBatchResult> {
	await Promise.resolve();
	void fromEngine;
	void fromScope;
	void move;
	let failedOnce = false;
	const rows: NgwaCopyRowResult[] = destinations.map((d) => {
		// Exercise one partial failure: first transcode→project destination.
		if (!failedOnce && d.mode === 'transcode' && d.scope.startsWith('project:')) {
			failedOnce = true;
			return {
				engine: d.engine,
				scope: d.scope,
				mode: d.mode,
				ok: false,
				error: {
					kind: 'io',
					path: `${d.scope}/.${d.engine}/${kind}s/${name}.toml`,
					message: 'mock partial-failure (WP-24 backend not yet wired)',
				},
			};
		}
		const fileBased = kind === 'skill' || kind === 'agent' || kind === 'command';
		const ext = d.mode === 'transcode' ? 'toml' : 'md';
		const scopeRoot =
			d.scope === 'workspace'
				? `~/.${d.engine}`
				: `${d.scope.slice('project:'.length)}/.${d.engine}`;
		return {
			engine: d.engine,
			scope: d.scope,
			mode: d.mode,
			ok: true,
			mutation: {
				kind,
				name,
				scope: d.scope,
				path: fileBased ? `${scopeRoot}/${kind}s/${name}.${ext}` : `${scopeRoot}/settings.json`,
				linkTarget: null,
			},
		};
	});
	return { rows };
}

/**
 * Copy (or move) a single source primitive into N (engine, scope) destinations
 * in one batch — the backend for the D-09 multi-destination checklist drawer.
 * Same-format destinations copy verbatim; `md→toml` destinations transcode via
 * the existing forward transcoder. Each destination writes atomically and
 * reports its own row result, so a partial failure never rolls back the rows
 * that succeeded.
 *
 * WP-24 owns the Rust `claude_primitive_copy_batch` handler; until it lands this
 * routes through the mock seam above (`NGWA_TRANSCODE_MOCK`).
 */
export async function ngwaCrossEngineCopy(
	fromEngine: EngineId,
	kind: ClaudeStoreKind,
	name: string,
	fromScope: ClaudeStoreScope,
	destinations: NgwaCopyDestination[],
	move = false
): Promise<NgwaCopyBatchResult> {
	if (NGWA_TRANSCODE_MOCK) {
		return ngwaCrossEngineCopyMock(fromEngine, kind, name, fromScope, destinations, move);
	}
	return invoke<NgwaCopyBatchResult>('claude_primitive_copy_batch', {
		fromEngine,
		kind,
		name,
		fromScope,
		destinations,
		move,
	});
}

// ─── Ọba registry — WP-04 dependent-aware safe delete ─────────────────────────
//
// The incident guardrail's FE surface. `obaDependents` feeds the detail-pane
// dependents list; `obaSafeDelete` runs the guarded delete (refuses external
// masters + masters with live dependents); `obaRelinkDependents` re-points the
// dependent symlinks before forgetting a master. Backend: claude_store.rs.

/** Outcome of a guarded delete. `verdict` is one of `unlinked` | `deleted` |
 *  `refused_external` | `refused_dependents`. On a refusal, `dependents` lists
 *  the live dependent paths the relink chooser offers to re-point. Mirrors the
 *  Rust `SafeDeleteOutcome`. */
export interface SafeDeleteOutcome {
	verdict: 'unlinked' | 'deleted' | 'refused_external' | 'refused_dependents';
	removed: boolean;
	dependents: string[];
	message: string;
}

/** Per-link result of a relink-all. Mirrors the Rust `RelinkRow`. */
export interface ObaRelinkRow {
	link: string;
	ok: boolean;
	error: string | null;
}

/**
 * Live dependents of a primitive's canonical master — symlinks across all
 * scopes/engines that resolve into it. Computed fresh from disk (never a stored
 * list), so it is correct even if `store/registry.json` is lost.
 */
export async function obaDependents(kind: ClaudeStoreKind, name: string): Promise<string[]> {
	return invoke<string[]>('oba_dependents', { kind, name });
}

/**
 * Guarded delete of a primitive's canonical master. Refuses an external master
 * (`managed:false`) and any master with live dependents; only a vault-managed
 * master with zero dependents is hard-deleted. NEVER `remove_dir_all`s a master
 * out from under its dependents (the data-loss-incident guardrail). On a
 * successful hard-delete the registry record is dropped.
 */
export async function obaSafeDelete(
	kind: ClaudeStoreKind,
	name: string
): Promise<SafeDeleteOutcome> {
	return invoke<SafeDeleteOutcome>('oba_safe_delete', { kind, name });
}

/**
 * Re-point dependent symlinks at a new master (relink-all), returning a per-link
 * result in request order. Used before forgetting an external master so no
 * dependent is left dangling.
 */
export async function obaRelinkDependents(
	dependents: string[],
	newMaster: string
): Promise<ObaRelinkRow[]> {
	return invoke<ObaRelinkRow[]>('oba_relink_dependents', { dependents, newMaster });
}

/**
 * Unlink ONE dependent placement (a symlink) by absolute path — `remove_file`,
 * never recurses into the master. Refuses a non-symlink (use {@link obaSafeDelete}
 * for a real master). Resolves `true` if a link was removed, `false` if absent.
 * Backs the D-01 "Unlink one placement" choice.
 */
export async function obaUnlinkOne(path: string): Promise<boolean> {
	return invoke<boolean>('oba_unlink_one', { path });
}

/**
 * Drop the registry record for a primitive — provenance only. Touches no files:
 * the master + every symlink stay on disk exactly as they are. Backs the D-01
 * "Forget from registry" choice. Resolves `true` if a record existed.
 */
export async function obaForget(kind: ClaudeStoreKind, name: string): Promise<boolean> {
	return invoke<boolean>('oba_forget', { kind, name });
}

/**
 * Back-fill the registry with EXTERNAL masters discovered in the live farm
 * (`managed:false`, real `canonicalPath`). Lets {@link obaSafeDelete} resolve a
 * real external canonical (so it can hit `refused_external`/`refused_dependents`)
 * instead of a nonexistent vault path. Resolves the number of records recorded.
 */
export async function obaBackfillRegistry(): Promise<number> {
	return invoke<number>('oba_backfill_registry', {});
}

// ─── Ọba Phase 2 — install from git / npx + update (WP-07/08/09) ──────────────

/** Result of an update check: recorded version vs latest at the remote. */
export interface UpdateStatus {
	/** Recorded resolved version (git SHA / npm-spec SHA); null if never resolved. */
	current: string | null;
	/** Latest resolved version at the remote. */
	latest: string | null;
	/** True when an update is available (`current !== latest`). */
	behind: boolean;
}

/**
 * Install a primitive from a git remote into the vault as a managed canonical
 * (`store/<kind>s/<name>`). Clones at `gitRef` (or default branch), records
 * provenance (`source:"git"`, resolved SHA). Does NOT place into any scope —
 * use {@link claudePrimitiveEnable} for that. File-based kinds only.
 */
export async function obaInstallGit(
	kind: ClaudeStoreKind,
	name: string,
	url: string,
	gitRef?: string | null,
	/** Phase 3: true when the install was discovered through the recommended
	 *  catalog (records `fromCatalog` + opts into auto-update). Omit/false for a
	 *  direct git install. */
	fromCatalog?: boolean
): Promise<ClaudeStoreEntry> {
	return invoke<ClaudeStoreEntry>('oba_install_git', {
		kind,
		name,
		url,
		gitRef: gitRef ?? null,
		fromCatalog: fromCatalog ?? false,
	});
}

/**
 * Install a skill via the Claude `skills` CLI (`npx skills add <spec>`) into the
 * vault as a managed canonical. Records `source:"npx"`. Skills only.
 * `fromCatalog` records catalog discovery (Phase 3) — set by the catalog Install
 * path, omitted for a direct npx install.
 */
export async function obaInstallNpx(
	kind: ClaudeStoreKind,
	name: string,
	spec: string,
	fromCatalog?: boolean
): Promise<ClaudeStoreEntry> {
	return invoke<ClaudeStoreEntry>('oba_install_npx', {
		kind,
		name,
		spec,
		fromCatalog: fromCatalog ?? false,
	});
}

/**
 * Install a multi-skill BUNDLE via the Claude `skills` CLI
 * (`npx skills add <spec> --skill '*'`). Materializes every member skill into
 * the vault canonical `store/bundles/<name>/<member>/` and returns the single
 * bundle registry record (`kind:"bundle"`, `members` populated with the sorted
 * member leaf names). Idempotent — re-running re-fetches + atomically swaps the
 * bundle dir + re-derives members, so this doubles as the update call.
 * `scope` is accepted for forward-compat with placement (WP-21) but is unused
 * today. `fromCatalog` records catalog discovery (Phase 3).
 */
export async function obaInstallBundle(
	name: string,
	spec: string,
	scope?: string | null,
	fromCatalog?: boolean
): Promise<ClaudeStoreEntry> {
	return invoke<ClaudeStoreEntry>('oba_install_bundle', {
		name,
		spec,
		scope: scope ?? null,
		fromCatalog: fromCatalog ?? false,
	});
}

/** A `(kind,name)` primitive identity. Mirrors the Rust `PrimitiveRef`
 *  (`claude_store/resolve.rs`). */
export interface PrimitiveRef {
	kind: string;
	name: string;
}

/** A catalog row the resolver-driven install uses to resolve a dependency
 *  `(kind,name)` to a fetchable `(source,url)`. Subset of `PrimitiveCatalogEntry`.
 *  Mirrors the Rust `CatalogEntryRef`. */
export interface CatalogEntryRef {
	kind: string;
	name: string;
	source: 'git' | 'npx';
	url: string;
	/** Member skills a `bundle` catalog row carries (WP-18/19). Absent for
	 *  non-bundle rows. Mirrors the Rust `CatalogEntryRef.members`. */
	members?: string[];
}

/** Result of a resolver-driven install (ADR-015 §3b / WP-14). Mirrors the Rust
 *  `InstallWithDepsResult`. */
export interface InstallWithDepsResult {
	/** The primitive the user asked to install. */
	target: ClaudeStoreEntry;
	/** The auto-installed dependency closure, deepest-first (enable order). */
	installed: ClaudeStoreEntry[];
	/** Deps that were already present (listed, not installed) — consent UX. */
	alreadySatisfied: PrimitiveRef[];
}

/**
 * Install a primitive AND its forward-dependency closure (ADR-015 §3b · WP-14).
 * The FE passes a catalog snapshot so each `requires` dependency resolves to a
 * fetchable source; the missing closure auto-installs transactionally (rolled
 * back with the target on any failure). Returns the target, the installed
 * closure (enable order), and the already-satisfied deps.
 */
export async function obaInstallWithDeps(
	kind: ClaudeStoreKind,
	name: string,
	source: 'git' | 'npx',
	url: string,
	catalog: CatalogEntryRef[],
	gitRef?: string | null,
	fromCatalog?: boolean
): Promise<InstallWithDepsResult> {
	return invoke<InstallWithDepsResult>('oba_install_with_deps', {
		kind,
		name,
		source,
		url,
		gitRef: gitRef ?? null,
		fromCatalog: fromCatalog ?? false,
		catalog,
	});
}

/**
 * Re-verify a primitive's `requires` at enable time (WP-14 re-verify-at-enable):
 * return the recorded deps that are no longer present (the FE offers to re-fetch
 * them). Empty ⇒ the closure is intact.
 */
export async function obaMissingRequires(
	kind: ClaudeStoreKind,
	name: string
): Promise<RequiresEntry[]> {
	return invoke<RequiresEntry[]>('oba_missing_requires', { kind, name });
}

/** One per-entry outcome of a batch auto-update run (Phase 3). Mirrors the Rust
 *  `AutoUpdateRow`. */
export interface AutoUpdateRow {
	kind: ClaudeStoreKind;
	name: string;
	/** `'updated'` | `'current'` | `'error'`. */
	status: 'updated' | 'current' | 'error';
	version: string | null;
	error: string | null;
}

/** Summary of a batch auto-update run (Phase 3). Mirrors the Rust
 *  `AutoUpdateSummary`. */
export interface AutoUpdateSummary {
	updated: AutoUpdateRow[];
	current: AutoUpdateRow[];
	errored: AutoUpdateRow[];
}

/**
 * Phase 3 — auto-update every `autoUpdate`-opted entry that's behind its remote.
 * FE-driven (call on the catalog surface mount). Per-entry errors are collected,
 * never abort the batch.
 */
export async function obaAutoUpdateAll(): Promise<AutoUpdateSummary> {
	return invoke<AutoUpdateSummary>('oba_auto_update_all', {});
}

/**
 * Phase 3 — toggle the per-entry auto-update opt-in (persists to registry.json).
 * Returns the new flag value.
 */
export async function obaSetAutoUpdate(
	kind: ClaudeStoreKind,
	name: string,
	enabled: boolean
): Promise<boolean> {
	return invoke<boolean>('oba_set_auto_update', { kind, name, enabled });
}

/**
 * Check whether a git/npx-installed primitive is behind its remote
 * (`git ls-remote` vs the recorded version). Read-only.
 */
export async function obaCheckUpdate(kind: ClaudeStoreKind, name: string): Promise<UpdateStatus> {
	return invoke<UpdateStatus>('oba_check_update', { kind, name });
}

/**
 * Re-fetch a managed primitive into its existing canonical IN PLACE (atomic
 * swap), so dependent symlinks resolve to the refreshed files with no relink.
 * Bumps the recorded version + `updatedAt`. Refuses an external master.
 */
export async function obaUpdate(kind: ClaudeStoreKind, name: string): Promise<ClaudeStoreEntry> {
	return invoke<ClaudeStoreEntry>('oba_update', { kind, name });
}

// ─── Iyke (phase 11 — Day 1: read-side state + shell mirror push) ─────────────

export interface IykeEndpoint {
	url: string;
	token: string;
	port: number;
}

export async function iykeEndpoint(): Promise<IykeEndpoint> {
	return invoke('iyke_endpoint');
}

/** DOM-tree probe result. `text` is the Playwright-style accessibility tree
 *  snapshot; `json` is the same data as a tree of `{ role, name, ref, value,
 *  children }` objects; `generation` bumps each snapshot so callers can
 *  detect stale refs. */
export interface IykeDomResult {
	text: string;
	json: unknown;
	generation: number;
}

/** Query the focused iframe's accessibility tree via the iyke bridge. Same
 *  mechanism as the `/iyke/dom` HTTP endpoint but exposed as a Tauri command
 *  so the Studio's right-rail DOM tab can refresh without round-tripping
 *  through localhost HTTP. `pane` is the leaf id; omit / "shell" for the
 *  main webview (Phase A only). */
export async function iykeDomQuery(args?: {
	query?: string;
	all?: boolean;
	pane?: string;
}): Promise<IykeDomResult> {
	return invoke('iyke_dom_query', {
		query: args?.query ?? null,
		all: args?.all ?? null,
		pane: args?.pane ?? null,
	});
}

export async function iykeSetShell(args: {
	mode?: string | null;
	route?: string | null;
}): Promise<void> {
	return invoke('iyke_set_shell', {
		mode: args.mode ?? null,
		route: args.route ?? null,
	});
}

// ─── Screenshots ──────────────────────────────────────────────────────────────

export interface ScreenshotResult {
	path: string;
	width: number;
	height: number;
	bytesLen: number;
}

export async function screenshotWindow(outPath?: string): Promise<ScreenshotResult> {
	return invoke('screenshot_window', { outPath: outPath ?? null });
}

export async function screenshotPane(paneId: string, outPath?: string): Promise<ScreenshotResult> {
	return invoke('screenshot_pane', {
		paneId,
		outPath: outPath ?? null,
	});
}

export interface ScreenshotConfig {
	/** User-supplied override (raw, may contain `~`). `null` = use platform default. */
	overrideDir: string | null;
	/** Per-platform default, absolute path. */
	defaultDir: string;
	/** What `capture()` will actually use right now. Tilde-expanded. */
	effectiveDir: string;
}

export async function screenshotGetConfig(): Promise<ScreenshotConfig> {
	return invoke('screenshot_get_config');
}

/** Pass `null` (or omit) to clear the override and revert to the platform default. */
export async function screenshotSetDir(dir: string | null): Promise<void> {
	return invoke('screenshot_set_dir', { dir: dir ?? null });
}

// ─── Spike: dynamic ACL verification (delete after kernel lands) ─────────

export async function spikeGrantFsRead(capabilityId: string, path: string): Promise<string> {
	return invoke<string>('spike_grant_fs_read', { capabilityId, path });
}

// ─── Pkg kernel ────────────────────────────────────────────────────────

/**
 * Provenance for an installed pkg. Recorded at install time by the kernel
 * and surfaced here so the UI can group / badge / gate uninstall on the
 * same source-of-truth that the kernel uses to enforce policy.
 *
 * Wire format mirrors `src-tauri/src/pkg/source.rs::InstallSource` —
 * `{kind}` plus per-variant fields. Keep in sync with that file.
 */
export type PkgInstallSource =
	| { kind: 'builtin' }
	| { kind: 'registry'; url: string; publisher_key: string | null }
	| { kind: 'local'; path: string };

export interface PkgInstalledSummary {
	id: string;
	version: string;
	ikenga_api: string;
	install_path: string;
	enabled: boolean;
	installed_at: number;
	compatible: boolean;
	source: PkgInstallSource;
	/** Phase 2 (projects-first-class): null/undefined = workspace scope
	 *  (always loaded); slug = project-scoped (loaded only when that
	 *  project is active). Optional so fixtures from before Phase 2
	 *  don't need a manual backfill. */
	project_id?: string | null;
}

export interface PkgKernelStatus {
	installed: PkgInstalledSummary[];
	registries: Record<string, unknown>;
	api_version: number;
}

export async function pkgInstallFromPath(
	installPath: string,
	scope?: PkgScopeWire | null
): Promise<{ installed: PkgInstalledSummary }> {
	return invoke('pkg_install_from_path', { installPath, scope: scope ?? null });
}

/** Phase 2 scope-picker wire format. `"workspace"` = always loaded;
 *  `"project:<id>"` = project-scoped; null/undefined = active project. */
export type PkgScopeWire = 'workspace' | `project:${string}`;

export async function pkgSetScope(pkgId: string, scope: PkgScopeWire | null): Promise<void> {
	return invoke('pkg_set_scope', { pkgId, scope });
}

/**
 * Install a pkg from a (registry-vetted) tarball URL. Rust downloads the
 * tarball, re-verifies SHA-512 against `integrity`, untars into the pkgs
 * dir, and hands off to the kernel as `InstallSource::Registry`.
 *
 * The TS registry-client is expected to have already signature-verified the
 * index that named this tarball — see `src/lib/registry/`.
 */
export interface PkgInstallFromRegistryArgs {
	tarball: string;
	integrity: string;
	pkgId: string;
	sourceUrl: string;
	/**
	 * Publisher's minisign public key for this pkg, as named by the signed
	 * registry index. Threaded into `InstallSource::Registry.publisher_key`
	 * and used by the Rust trust gate (`pkg::signature`) to verify the
	 * manifest's `signature` at install + every boot. The signed index does
	 * not carry per-pkg publisher keys yet (WP-06), so this is `undefined`
	 * today — the field is wired now so callers don't change shape when keys
	 * land. Absent ⇒ the pkg installs/runs but is never trusted for elevated
	 * host capabilities.
	 */
	publisherKey?: string | null;
}

export async function pkgInstallFromRegistry(
	args: PkgInstallFromRegistryArgs,
	scope?: PkgScopeWire | null
): Promise<{ installed: PkgInstalledSummary }> {
	return invoke('pkg_install_from_registry', { args, scope: scope ?? null });
}

export async function pkgUninstall(pkgId: string): Promise<void> {
	return invoke('pkg_uninstall', { pkgId });
}

export async function pkgSetEnabled(pkgId: string, enabled: boolean): Promise<void> {
	return invoke('pkg_set_enabled', { pkgId, enabled });
}

export async function pkgKernelStatus(): Promise<PkgKernelStatus> {
	return invoke<PkgKernelStatus>('pkg_kernel_status');
}

// ─── skill actions (WP-13) ────────────────────────────────────────────────
//
// A `kind: skill` pkg contributes actions under
// `<skills_dir>/<skill>/actions/*.md`. The Rust `list_skill_actions` command
// parses each file's YAML frontmatter and returns this flat shape. Only
// `uxMode === 'confirm'` actions dispatch in WP-13; the rest render disabled.

/** UX dispatch modes a skill action can declare. Only `confirm` is wired
 *  end-to-end in WP-13; the rest render as disabled placeholders. */
export type SkillActionUxMode = 'confirm' | 'streaming' | 'approve' | (string & {});

/** A trigger that can invoke a skill action — mirrors the Rust `SkillTrigger`
 *  (the `Trigger` discriminated union in `@ikenga/contract`). */
export interface SkillTrigger {
	kind: 'manual' | 'schedule' | 'webhook' | 'event' | (string & {});
	cron?: string;
	label?: string;
	path?: string;
	event?: string;
}

/** The setup lifecycle block — present only on the `setup` action. Mirrors the
 *  Rust `SkillSetup` / the contract `SetupSpec`. */
export interface SkillSetup {
	mode: 'ai_infer' | 'interview' | (string & {});
	templateVersion: number;
	inferSources?: string[];
	interviewQuestions?: string[];
}

/** A single skill action — mirrors the Rust `SkillAction` (camelCase serde). */
export interface SkillAction {
	pkgId: string;
	skill: string;
	verb: string;
	name: string;
	description?: string;
	domain?: string;
	uxMode: SkillActionUxMode;
	runKind?: string;
	promptTemplate?: string;
	inputsSchemaJson?: string;
	dependsOn?: string[];
	triggers?: SkillTrigger[];
	requiresCapabilities?: string[];
	setup?: SkillSetup;
}

/** List the skill actions contributed by a single installed pkg. Returns an
 *  empty array for non-skill pkgs or pkgs without a `skills` dir. */
export async function listSkillActions(pkgId: string): Promise<SkillAction[]> {
	return invoke<SkillAction[]>('list_skill_actions', { pkgId });
}

/** List skill actions across every installed pkg. */
export async function listAllSkillActions(): Promise<SkillAction[]> {
	return invoke<SkillAction[]>('list_all_skill_actions');
}

export interface PkgDiscovered {
	id: string;
	name: string;
	version: string;
	install_path: string;
	valid: boolean;
	error: string | null;
	installed: boolean;
	compatible: boolean;
}

/**
 * Dev-mode helper: scan a workspace directory for sibling pkgs without
 * installing anything. Pass `workspaceDir` explicitly, or omit to fall back
 * to the `IKENGA_WORKSPACE_DIR` env var on the Rust side. Returns an empty
 * list when neither is set.
 */
export async function pkgDiscoverWorkspace(workspaceDir?: string): Promise<PkgDiscovered[]> {
	return invoke<PkgDiscovered[]>('pkg_discover_workspace', { workspaceDir });
}

/**
 * Restart a supervised pkg's sidecar. Resets Blocked / Crashed / Parked
 * back to Spawning and breaks any pending retry sleep so the supervisor
 * re-spawns immediately. Returns true if the pkg is supervised here, false
 * if no supervisor entry exists for that id (per-call lifecycle pkgs).
 */
export async function pkgSupervisorRestart(pkgId: string): Promise<boolean> {
	return invoke<boolean>('pkg_supervisor_restart', { pkgId });
}

// ── Phase 9: pkg trust gating ─────────────────────────────────────────────

export interface PkgTrustPermsSummary {
	shell_execute: string[];
	fs_write_outside_sandbox: string[];
	net: string[];
	vault_keys: string[];
}

export type PkgTrustChangeReason =
	| { kind: 'never' }
	| { kind: 'permissions_changed'; prior_version: string; added: string[]; removed: string[] }
	| { kind: 'revoked' };

export type PkgTrustState = 'auto_trusted' | 'auto_granted' | 'granted' | 'needs_approval';

export interface PkgTrustEntry {
	pkg_id: string;
	version: string;
	state: PkgTrustState;
	perms: PkgTrustPermsSummary;
	last_granted_at_ms: number | null;
	change_reason: PkgTrustChangeReason | null;
	auto_trusted: boolean;
}

export interface PkgTrustPreview {
	pkg_id: string;
	version: string;
	perms: PkgTrustPermsSummary;
}

/**
 * List trust state for every installed pkg. Includes auto-trusted
 * builtins and skill-pack-only auto-grants alongside explicit grants and
 * pending approvals. Used by Settings → Pkgs Trust column.
 */
export async function pkgTrustList(): Promise<PkgTrustEntry[]> {
	return invoke<PkgTrustEntry[]>('pkg_trust_list');
}

/**
 * Per-pkg sensitive perms summary, fetched lazily when the Review dialog
 * opens. Returns the same shape as the `perms` field on a list entry.
 */
export async function pkgTrustPreview(pkgId: string): Promise<PkgTrustPreview> {
	return invoke<PkgTrustPreview>('pkg_trust_preview', { pkgId });
}

/**
 * Approve the pkg's *current* manifest sensitive-perms set. The version
 * string defends against a manifest update racing the dialog open; mismatch
 * rejects with an error so the FE can re-open with fresh data.
 */
export async function pkgTrustGrant(pkgId: string, version: string): Promise<void> {
	await invoke<void>('pkg_trust_grant', { pkgId, version });
}

/**
 * Revoke an existing trust grant. Subsequent MCP tools/call against this
 * pkg returns the structured `trust_required` error until re-granted.
 */
export async function pkgTrustRevoke(pkgId: string): Promise<void> {
	await invoke<void>('pkg_trust_revoke', { pkgId });
}

// ── Trust-review modal (2026-05-15) — capability-diff batch surface ──────
//
// Distinct from the Phase 9 sensitive-perms trust surface above. These
// commands gate boot-time capability changes (the FULL capabilities +
// permissions blocks) by parking the pkg out of the kernel's registry
// replay until the user approves or rejects the diff.

export interface PkgTrustReview {
	pkg_id: string;
	manifest_version: string;
	old_capabilities: string;
	new_capabilities: string;
	prior_approved_at_ms: number;
}

/**
 * List pkgs whose normalized capabilities + permissions differ from
 * their last-approved snapshot. Empty list = nothing to review.
 */
export async function pkgTrustListPending(): Promise<PkgTrustReview[]> {
	return invoke<PkgTrustReview[]>('pkg_trust_list_pending');
}

/**
 * Approve the current manifest's capabilities + permissions: write a new
 * explicit snapshot and re-register the pkg with the kernel (which boots
 * its sidecars / MCPs).
 */
export async function pkgTrustApprove(pkgId: string): Promise<void> {
	await invoke<void>('pkg_trust_approve', { pkgId });
}

/**
 * Reject the diff: delegate to the standard uninstall path.
 */
export async function pkgTrustReject(pkgId: string): Promise<void> {
	await invoke<void>('pkg_trust_reject', { pkgId });
}

// ─────────────────────────────────────────────────────────────────────────
// Runtime-ACL violations audit (2026-05-15)
// ─────────────────────────────────────────────────────────────────────────

export interface PkgPermissionViolation {
	id: number;
	pkg_id: string;
	scope_kind: string;
	attempted: string;
	declared: string;
	occurred_at: number;
}

/**
 * List permission-violation audit rows newest-first. `pkgId` filters to one
 * pkg; omit for cross-pkg counts (the Settings overview badge). `limit`
 * defaults to 100 and is hard-capped at 1000 server-side.
 */
export async function pkgPermissionViolationsList(
	pkgId?: string,
	limit?: number
): Promise<PkgPermissionViolation[]> {
	return invoke<PkgPermissionViolation[]>('pkg_permission_violations_list', {
		pkgId: pkgId ?? null,
		limit: limit ?? null,
	});
}

/**
 * Delete the named pkg's audit rows. Audit-only — does not alter trust
 * state or re-grant anything. Returns the number of rows removed.
 */
export async function pkgPermissionViolationsClear(pkgId: string): Promise<number> {
	return invoke<number>('pkg_permission_violations_clear', { pkgId });
}

/**
 * Debug-only: pre-bind a port so the smoke route can verify the supervised
 * sidecar transitions to Blocked when its dev-server child sees EADDRINUSE.
 * Pair with `devReleasePort(token)` to free the port and observe recovery.
 * Release builds reject with an error.
 */
export async function devBindPort(port: number): Promise<number> {
	return invoke<number>('dev_bind_port', { port });
}

export async function devReleasePort(token: number): Promise<boolean> {
	return invoke<boolean>('dev_release_port', { token });
}

export interface PkgDbDiag {
	db_path: string;
	pkg_installed_count: number;
	ids: string[];
}

export async function pkgDbDiag(): Promise<PkgDbDiag> {
	return invoke<PkgDbDiag>('pkg_db_diag');
}

// ─── pkg health (install-integrity check + cleanup) ──────────────────────────
// Mirrors the Rust `PkgHealthIssue` / `HealthIssueKind` serde (kernel.rs) — keep
// in lockstep. `issue.kind` is the tagged-union discriminant.
export type PkgHealthIssueKind =
	| { kind: 'manifest_missing' }
	| { kind: 'manifest_unreadable' }
	| { kind: 'manifest_unparseable' }
	| { kind: 'api_incompatible'; ikenga_api: string }
	| { kind: 'orphan_row'; table: string };

export interface PkgHealthIssue {
	id: string;
	install_path: string;
	enabled: boolean;
	issue: PkgHealthIssueKind;
	detail: string;
}

export interface PkgHealthRemoveAllResult {
	removed_records: number;
	removed_orphans: number;
}

/** Scan for broken / orphaned package install records (read-only). */
export async function pkgHealthScan(): Promise<PkgHealthIssue[]> {
	return invoke<PkgHealthIssue[]>('pkg_health_scan');
}

/** Remove one broken install record (its `pkg_installed` row + child rows). */
export async function pkgHealthRemove(pkgId: string): Promise<void> {
	return invoke('pkg_health_remove', { pkgId });
}

/** Remove every currently-detected broken record + orphan row. */
export async function pkgHealthRemoveAll(): Promise<PkgHealthRemoveAllResult> {
	return invoke<PkgHealthRemoveAllResult>('pkg_health_remove_all');
}

// ─── data health (Atelier/PA domain soft-FK orphan audit) ────────────────────
// Mirrors the Rust `OrphanReport` serde (commands/data_health.rs) — keep in
// lockstep. Read-only: the `0025`–`0054` migrations declare zero FK constraints,
// so cross-domain links are plain TEXT "soft links". This surfaces child rows
// whose non-null soft-FK value has no matching parent (a dangling reference).
// Never mutates — these are real business records; the user decides the fix.
export interface OrphanReport {
	/** The child table holding the dangling references. */
	table: string;
	/** The soft-FK column whose value has no matching parent. */
	column: string;
	/** The table the column conceptually references (`parent_table.id`). */
	parent_table: string;
	/** How many child rows have a non-null value absent from the parent. */
	orphan_count: number;
	/** Up to ~5 child-row ids, for locating the affected records. */
	sample_ids: string[];
}

/** Scan the domain soft-links for dangling references (read-only). Returns one
 *  report per soft-link that currently has one or more orphans; clean links are
 *  omitted (the FE renders those as a green check). */
export async function dataHealthScan(): Promise<OrphanReport[]> {
	return invoke<OrphanReport[]>('data_health_scan');
}

export interface PkgSettingsField {
	key: string;
	type: string;
	label: string;
	default?: unknown;
	description?: string | null;
}

export interface PkgSettingsSnapshot {
	pkg_id: string;
	/** Array of declared fields, or null if the pkg has no settings block. */
	schema: PkgSettingsField[] | null;
	/** Object of `{ key: parsed_value }` pulled from `pkg_settings`. */
	values: Record<string, unknown>;
}

export async function pkgSettingsGet(pkgId: string): Promise<PkgSettingsSnapshot> {
	return invoke<PkgSettingsSnapshot>('pkg_settings_get', { pkgId });
}

export async function pkgSettingsSet(pkgId: string, key: string, value: unknown): Promise<void> {
	return invoke('pkg_settings_set', { pkgId, key, value });
}

/** Mirrors the Rust `ActivityBarBadge` in `pkg/registries/activity_bar.rs`
 *  (WP-11). `null`/`undefined` on the call clears the badge. */
export interface PkgActivityBarBadge {
	dot: boolean;
	count?: number | null;
	tooltip?: string | null;
}

/** Set (or clear, with `null`) the status badge on a pkg's own activity-bar
 *  rail icon. Backs the `host.pkg.setBadge` AppBridge verb; errors if the
 *  pkg has no rail entry (no `ui.nav[0]`, or not yet registered). */
export async function pkgActivityBarSetBadge(
	pkgId: string,
	badge: PkgActivityBarBadge | null
): Promise<void> {
	return invoke('pkg_activity_bar_set_badge', { pkgId, badge });
}

/** Parsed manifest as raw JSON. Includes whatever optional blocks the
 *  manifest declared: permissions, settings, mcp, sidecars, ui, etc. */
export interface PkgManifestScreenshot {
	path: string;
	caption?: string | null;
}

export interface PkgManifestPreview {
	id: string;
	name: string;
	version: string;
	ikenga_api: string;
	kind?: string | null;
	permissions?: Record<string, unknown>;
	settings?: { schema?: PkgSettingsField[] };
	mcp?: Array<{ name: string; command: string; args?: string[] }>;
	sidecars?: Array<{ name: string; bin: string }>;
	cron?: Array<{ id: string; expr: string; handler: string }>;
	ui?: { routes?: Array<{ path: string; kind: string; source: string }> };
	skills?: string | null;
	commands?: string | null;
	agents?: string | null;
	screenshots?: PkgManifestScreenshot[];
	[key: string]: unknown;
}

export async function pkgPreviewManifest(installPath: string): Promise<PkgManifestPreview> {
	return invoke<PkgManifestPreview>('pkg_preview_manifest', { installPath });
}

/**
 * Read a pkg-declared screenshot and return a base64 data URL. `path` must
 * match one declared in the pkg's `manifest.screenshots[].path`; the kernel
 * resolves it against the pkg's install_path and rejects `../` escapes.
 * Result is cacheable indefinitely (immutable per pkg version).
 */
export async function pkgScreenshot(pkgId: string, path: string): Promise<string> {
	return invoke<string>('pkg_screenshot', { pkgId, path });
}

// ─── Pkg content (iframe mount) ─────────────────────────────────────────
//
// `pkgContentUrl` mints a per-iframe access token and returns the URL the
// iframe should load (already includes pkgId + token + trailing slash).
// Append the manifest's `ui.routes[].source` (e.g. `dist/index.html` →
// pass just the filename relative to dist) to construct the final src.
// `pkgContentRevoke` releases the token when the iframe unmounts.

export interface PkgContentHandle {
	url: string;
	token: string;
}

export async function pkgContentUrl(pkgId: string): Promise<PkgContentHandle> {
	return invoke<PkgContentHandle>('pkg_content_url', { pkgId });
}

// `pkgContentHtml` reads the iframe entry HTML from the pkg's dist/, mints
// an access token, and injects a `<base href>` so subresource loads resolve
// against `http://127.0.0.1:<port>/<pkgId>/<token>/`. The returned `html`
// is meant to be assigned to `<iframe srcdoc>` — that's the workaround for
// the documented WebKitGTK bug where iframe-document loads from any
// non-https origin (custom protocol or http loopback) get blocked even
// though subresource fetches succeed.
// See https://github.com/tauri-apps/tauri/issues/12767.
export interface PkgContentHtmlHandle {
	html: string;
	baseUrl: string;
	token: string;
	/** Resolved Supabase config when the pkg's manifest declared
	 *  `capabilities.supabase`. `null` when the pkg didn't declare it, or
	 *  declared it non-required and the vault has no keys. Pkgs that don't
	 *  declare the capability never see this field populated. */
	supabase: { url: string; anonKey: string } | null;
	/** Resolved named secrets (ADR-017) when the pkg declared
	 *  `capabilities.secrets` AND is trusted-for-elevated. `values` maps each
	 *  declared `name` → its resolved plaintext; `missing` lists declared,
	 *  non-required names absent from the vault. `null` when the pkg didn't
	 *  declare the cap OR isn't trusted (fail-closed — silently ignored). The
	 *  iframe never sees a `vault_key`. */
	secrets: { values: Record<string, string>; missing: string[] } | null;
}

export async function pkgContentHtml(pkgId: string, source: string): Promise<PkgContentHtmlHandle> {
	return invoke<PkgContentHtmlHandle>('pkg_content_html', { pkgId, source });
}

/** The single FE gate for ELEVATED host capabilities (ADR-017 / trusted-pkg
 *  tier). Returns true only when the pkg is trusted-for-elevated
 *  (`TrustState::AutoTrusted` — builtin provenance, `ikenga dev`, or a
 *  signature-verified registry pkg). Wave-2 elevated verbs (`host.fetch` /
 *  `host.invoke`, WP-04/05) call this in `dispatchHostCall` as
 *  `pkgDeclaresCapability(pkgId, '<cap>') && pkgIsTrustedForElevated(pkgId)`;
 *  the Rust command re-checks the same gate (the FE check is fail-fast UX
 *  only — a hostile iframe could skip it). Fail-closed: un-installed /
 *  un-loadable / un-evaluable → false. */
export async function pkgIsTrustedForElevated(pkgId: string): Promise<boolean> {
	return invoke<boolean>('pkg_is_trusted_for_elevated', { pkgId });
}

// ─── host.fetch — mediated outbound HTTP proxy (ADR-017, WP-04) ──────────
//
// A TRUSTED pkg names a URL + request shape; the shell makes the request,
// attaches the auth credential from Stronghold, and returns the credential-
// free response. ALL enforcement (URL allowlist via permissions.net, SSRF
// guard, redirect handling, size cap, credential injection) is Rust-side —
// the FE only validates arg shape. The auth secret NEVER enters the iframe.

/** Request shape for `pkg_fetch`. `url` must match a `permissions.net` glob.
 *  The auth header is NOT supplied here — the shell injects it from the named
 *  `capabilities.http.auth_secret`. */
export interface PkgFetchReq {
	url: string;
	method?: string;
	headers?: Record<string, string>;
	/** String sent verbatim, or any JSON value (serialized host-side). */
	body?: string | Record<string, unknown> | unknown[];
	/** Per-call timeout (ms), clamped host-side to [1, 60000]. */
	timeout?: number;
}

/** Frozen result envelope. A non-2xx HTTP `status` is still `ok: true` — only
 *  gate/guard/transport failures set `ok: false` + a frozen `reason`. The
 *  injected auth header is never echoed back (only response headers, with
 *  Set-Cookie stripped). */
export interface PkgFetchResult {
	ok: boolean;
	status?: number;
	headers?: Record<string, string>;
	body?: string;
	truncated?: boolean;
	bytes?: number;
	reason?: string;
}

export async function pkgFetch(pkgId: string, req: PkgFetchReq): Promise<PkgFetchResult> {
	return invoke<PkgFetchResult>('pkg_fetch', { pkgId, req });
}

// ─── host.invoke — scoped named-command passthrough (ADR-017, WP-05) ──────
//
// A TRUSTED pkg runs a small allowlist of NAMED commands declared in
// `capabilities.invoke.commands` (D-06: invoke's OWN field, not
// permissions["shell.execute"]). Not a general shell. Rust re-checks trust +
// the allowlist; the FE check is fail-fast UX only.

/** Result of a `pkg_invoke` named-command run. */
export interface PkgInvokeResult {
	ok: boolean;
	error?: string;
	stdout?: string;
	stderr?: string;
	exitCode?: number;
	timedOut?: boolean;
}

export async function pkgInvoke(
	pkgId: string,
	command: string,
	args: string[]
): Promise<PkgInvokeResult> {
	return invoke<PkgInvokeResult>('pkg_invoke', { pkgId, command, args });
}

export async function pkgContentRevoke(token: string): Promise<void> {
	return invoke('pkg_content_revoke', { token });
}

/** Per-folder Studio project-access trust gate (WP-04). Canonicalizes `path`,
 *  returns `{ granted: true }` immediately if `com.ikenga.studio` already holds
 *  a grant for it, otherwise pops the native trust prompt and awaits the user's
 *  decision. Backs the `host.openFolder` iframe verb. */
export async function pkgStudioRequestProjectAccess(path: string): Promise<{ granted: boolean }> {
	return invoke<{ granted: boolean }>('pkg_studio_request_project_access', { path });
}

// ─── Pkg child-webview panes (Phase 1) ──────────────────────────────────
//
// Native webview surfaces owned by the kernel and rendered as a child of
// the main Tauri window. The React side mounts a placeholder div
// (`PkgWebviewHost`) that measures its DOM rect and asks the kernel to
// create / move / destroy a webview at that rect. The webview floats over
// the React tree natively; navigation, eval, and cookie isolation are
// driven by the kernel + the pkg's MCP server, not by React.
//
// Backed by `src-tauri/src/commands/pkg_webview.rs` and the
// `WebviewPanesRegistry` (`src-tauri/src/pkg/webview.rs`). The `eval` and
// `set_visible` paths exist in Rust but are intentionally NOT exposed to
// the FE — only the kernel and pkg-MCP servers ever drive them.

export interface PkgWebviewRect {
	x: number;
	y: number;
	w: number;
	h: number;
}

export interface PkgWebviewCreateResult {
	/** Internal label assigned by the kernel (e.g. `pkg-com-ikenga-browser-spotify`). Opaque. */
	webviewLabel: string;
}

/** Create a child webview for `(pkgId, paneId)` at `rect`, loading `url`.
 *  `partition` selects a per-pkg cookie jar (must be one of the partitions
 *  declared in the pkg's `capabilities.webview.partitions`); omit / null
 *  for the default jar. The returned label is opaque — the React side
 *  doesn't need it (subsequent ops are keyed on `(pkgId, paneId)`), but
 *  it's surfaced for logging / debugging. */
export async function pkgWebviewCreate(
	pkgId: string,
	paneId: string,
	url: string,
	rect: PkgWebviewRect,
	partition?: string | null
): Promise<PkgWebviewCreateResult> {
	// Rust-side `PkgWebviewCreateResult` carries `#[serde(rename_all = "camelCase")]`
	// so the wire format is `webviewLabel` already — no normalization needed.
	return invoke<PkgWebviewCreateResult>('pkg_webview_create', {
		pkgId,
		paneId,
		url,
		rect,
		partition: partition ?? null,
	});
}

export async function pkgWebviewDestroy(pkgId: string, paneId: string): Promise<void> {
	return invoke('pkg_webview_destroy', { pkgId, paneId });
}

export async function pkgWebviewSetRect(
	pkgId: string,
	paneId: string,
	rect: PkgWebviewRect
): Promise<void> {
	return invoke('pkg_webview_set_rect', { pkgId, paneId, rect });
}

export async function pkgWebviewNavigate(
	pkgId: string,
	paneId: string,
	url: string
): Promise<void> {
	return invoke('pkg_webview_navigate', { pkgId, paneId, url });
}

// ─── Pkg MCP tool routing ───────────────────────────────────────────────
//
// v1 stub. The host bridge calls this when the iframe fires an MCP
// `tools/call`. Returns `{ ok: false, error: "not_implemented" }` until
// the first sidecar-owning pkg lands; the protocol path is exercised
// end-to-end so spec compliance is testable now.

export interface PkgMcpCallResult {
	ok: boolean;
	error: string | null;
	result: unknown | null;
}

export async function pkgMcpCall(
	pkgId: string,
	tool: string,
	args: unknown
): Promise<PkgMcpCallResult> {
	return invoke<PkgMcpCallResult>('pkg_mcp_call', { pkgId, tool, args });
}

// ─── pkg sidecar invocation ───────────────────────────────────────────────────
//
// Invoke a package-declared sidecar binary one-shot. Counterpart to the
// `sidecars: [{ name, bin }]` manifest block. Returns captured stdout +
// stderr + exit_code. The pkg_id must own the named sidecar; cross-pkg
// invocations are rejected at the Rust side.
//
// Used by pkg iframes (via the AppBridge) and by the cron registry's
// `sidecar:<name> <sub>` handler.

export interface PkgSidecarCallResult {
	ok: boolean;
	error: string | null;
	stdout: string | null;
	stderr: string | null;
	exit_code: number | null;
	timed_out: boolean;
}

export async function pkgSidecarCall(
	pkgId: string,
	name: string,
	args: string[] = [],
	options: { stdin?: string; timeoutSecs?: number } = {}
): Promise<PkgSidecarCallResult> {
	return invoke<PkgSidecarCallResult>('pkg_sidecar_call', {
		pkgId,
		name,
		args,
		stdin: options.stdin ?? null,
		timeoutSecs: options.timeoutSecs ?? null,
	});
}

// ─── Streaming sidecar RPC ───────────────────────────────────────────────────
//
// Companion to `pkgSidecarCall` (one-shot) for long-lived sidecars that
// maintain in-memory state across requests (language servers, daemons).
// First send lazily spawns the child; subsequent sends reuse it. Lines
// from the child's stdout arrive as `pkg://sidecar/<pkgId>/<name>/message`
// Tauri events; child exit fires `pkg://sidecar/<pkgId>/<name>/exit`.

export async function pkgSidecarRpcSend(
	pkgId: string,
	name: string,
	message: string
): Promise<void> {
	return invoke<void>('pkg_sidecar_rpc_send', { pkgId, name, message });
}

export async function pkgSidecarRpcShutdown(pkgId: string, name: string): Promise<boolean> {
	return invoke<boolean>('pkg_sidecar_rpc_shutdown', { pkgId, name });
}

export function pkgSidecarMessageEvent(pkgId: string, name: string): string {
	return `pkg://sidecar/${pkgId.replace(/\./g, '_')}/${name.replace(/\./g, '_')}/message`;
}

export function pkgSidecarExitEvent(pkgId: string, name: string): string {
	return `pkg://sidecar/${pkgId.replace(/\./g, '_')}/${name.replace(/\./g, '_')}/exit`;
}

// ─── Iyke MCP info (settings panel) ───────────────────────────────────────────
//
// Resolves the absolute path of the bundled iyke-mcp binary so the settings
// panel can render a copy-to-clipboard MCP-client config snippet.

export interface IykeMcpInfo {
	// Absolute path the user should configure their MCP client against.
	path: string;
	// True when the binary actually exists at `path`.
	present: boolean;
	// "dev-tree" | "resource-dir" | "unknown" — surfaced for the help text.
	source: string;
}

export async function iykeMcpInfo(): Promise<IykeMcpInfo> {
	return invoke<IykeMcpInfo>('iyke_mcp_info');
}

// ─── Activity-bar pinning (user-level) ────────────────────────────────────────
//
// Two distinct domains:
//
//   * **Sections** are user-created groups for the pinned area of the
//     activity bar. Reserved ids (`system`, `settings`) are host-owned and
//     rejected by Rust.
//   * **Pins** are user pins of artifacts/routes/files/etc. They may belong
//     to a section (`sectionId`) or sit section-less (`null`). Reorder is
//     scoped to a single section — pass `''` (empty string) as `sectionId`
//     to the reorder command to mean "no section".

export type ActivityPinKind = 'artifact' | 'route' | 'file' | 'external' | 'pkg-route';

export interface ActivityPin {
	id: string;
	kind: ActivityPinKind;
	target: string;
	label: string;
	iconLucide: string | null;
	iconEmoji: string | null;
	sectionId: string | null;
	sortOrder: number;
	createdAt: string;
	/** Stable artifact id from the manifest. Lookup key for
	 *  `ikenga://artifact/<id>`. NULL on non-artifact pins and on artifact
	 *  pins authored without an `id` field. */
	manifestId: string | null;
	/** ISO-8601 UTC timestamp of the most recent open. NULL until the pin is
	 *  first opened via the resolver. */
	lastOpenedAt: string | null;
}

export interface ActivitySection {
	id: string;
	label: string;
	iconLucide: string | null;
	iconEmoji: string | null;
	sortOrder: number;
	createdAt: string;
}

export async function activityPinsList(): Promise<ActivityPin[]> {
	return invoke<ActivityPin[]>('activity_pins_list');
}

export async function activityPinsAdd(args: {
	kind: ActivityPinKind;
	target: string;
	label: string;
	iconLucide?: string | null;
	iconEmoji?: string | null;
	sectionId?: string | null;
	/** Stable artifact id (from `<script id="ikenga-manifest">`). Required
	 *  for `ikenga://artifact/<id>` to resolve back to this pin; optional
	 *  otherwise. */
	manifestId?: string | null;
}): Promise<ActivityPin> {
	return invoke<ActivityPin>('activity_pins_add', {
		kind: args.kind,
		target: args.target,
		label: args.label,
		iconLucide: args.iconLucide ?? null,
		iconEmoji: args.iconEmoji ?? null,
		sectionId: args.sectionId ?? null,
		manifestId: args.manifestId ?? null,
	});
}

export async function activityPinsRemove(id: string): Promise<void> {
	return invoke('activity_pins_remove', { id });
}

/** Reorder pins within a section. Pass `''` as `sectionId` for the
 *  section-less group. The Rust side validates non-empty ids. */
export async function activityPinsReorder(orderedIds: string[], sectionId: string): Promise<void> {
	return invoke('activity_pins_reorder', { orderedIds, sectionId });
}

/** Look up a pinned artifact by its manifest id. Returns null when no pin
 *  claims this id. Read-only — use `activityPinsTouchOpen` to bump recency
 *  after the artifact actually mounts. */
export async function activityPinsResolveArtifact(manifestId: string): Promise<ActivityPin | null> {
	return invoke<ActivityPin | null>('activity_pins_resolve_artifact', { manifestId });
}

/** Stamp `lastOpenedAt` to "now" for a pin. Fire-and-forget after a
 *  successful artifact open. */
export async function activityPinsTouchOpen(pinId: string): Promise<void> {
	return invoke('activity_pins_touch_open', { pinId });
}

export async function activitySectionsList(): Promise<ActivitySection[]> {
	return invoke<ActivitySection[]>('activity_sections_list');
}

export async function activitySectionsCreate(args: {
	id: string;
	label: string;
	iconLucide?: string | null;
	iconEmoji?: string | null;
}): Promise<ActivitySection> {
	return invoke<ActivitySection>('activity_sections_create', {
		id: args.id,
		label: args.label,
		iconLucide: args.iconLucide ?? null,
		iconEmoji: args.iconEmoji ?? null,
	});
}

export async function activitySectionsUpdate(args: {
	id: string;
	label?: string;
	/** `undefined` = leave unchanged. `null` = clear. `string` = set. */
	iconLucide?: string | null;
	iconEmoji?: string | null;
}): Promise<ActivitySection> {
	// Distinguish "not supplied" (don't touch the column) from "explicit null"
	// (clear it) by serializing both cases differently. Rust deserializes
	// `Option<Option<String>>`: outer None = leave, inner None = NULL.
	const payload: Record<string, unknown> = { id: args.id };
	if (args.label !== undefined) payload.label = args.label;
	if (Object.hasOwn(args, 'iconLucide')) {
		payload.iconLucide = args.iconLucide ?? null;
	}
	if (Object.hasOwn(args, 'iconEmoji')) {
		payload.iconEmoji = args.iconEmoji ?? null;
	}
	return invoke<ActivitySection>('activity_sections_update', payload);
}

export async function activitySectionsRemove(id: string): Promise<void> {
	return invoke('activity_sections_remove', { id });
}

// ─── Backup / restore ─────────────────────────────────────────────────────────
//
// Phase 2: SQLite + age-passphrase-encrypted secrets + installed-pkg list.
// Restore is stage-and-swap-on-boot. `backupImport` writes a staged db,
// optionally a chmod-0600 secrets-pending.json (decrypted at import-time),
// and a marker; the next app launch swaps ikenga.db before any pool opens, and
// replays staged secrets through the Stronghold-backed `bulk_set`.

export interface PkgEntry {
	id: string;
	version: string;
	enabled: boolean;
}

export type PathMode = 'raw' | 'tokenized' | 'bundled';

export interface PathWarning {
	table: string;
	column: string;
	value: string;
	reason: string;
}

export interface BackupManifest {
	format_version: number;
	schema_version: number;
	created_at: string;
	hostname: string;
	username: string;
	path_mode: PathMode;
	/** Export-time $HOME, present iff path_mode === "tokenized". */
	home_dir: string | null;
	has_secrets: boolean;
	pkg_count: number;
	path_warnings: PathWarning[];
}

export interface BackupSummary {
	path: string;
	created_at: string;
	size_bytes: number;
	schema_version: number;
	has_secrets: boolean;
	pkg_count: number;
	path_mode: PathMode;
}

export interface ExportResult {
	path: string;
	size_bytes: number;
	secrets_count: number;
	pkg_count: number;
	path_warnings_count: number;
}

export type SchemaAction =
	| { kind: 'match' }
	| { kind: 'forward'; from: number; to: number }
	| { kind: 'newer_than_app'; backup: number; app: number };

export interface ImportPreview {
	manifest: BackupManifest;
	size_bytes: number;
	schema_action: SchemaAction;
	pkgs: PkgEntry[];
}

export interface ImportResult {
	staged_at: string;
	requires_restart: boolean;
	secrets_staged: boolean;
}

export interface BackupExportOpts {
	/** When true, the bundle includes vault secrets encrypted with `passphrase`. */
	includeSecrets: boolean;
	/** Required when `includeSecrets` is true. */
	passphrase?: string;
	/** Defaults to "raw" on the Rust side. "bundled" is not yet implemented. */
	pathMode?: PathMode;
}

export async function backupExport(
	destPath: string,
	opts: BackupExportOpts
): Promise<ExportResult> {
	return invoke<ExportResult>('backup_export', {
		destPath,
		includeSecrets: opts.includeSecrets,
		passphrase: opts.passphrase ?? null,
		pathMode: opts.pathMode ?? null,
	});
}

export async function backupImport(
	srcPath: string,
	opts: { dryRun: boolean; passphrase?: string }
): Promise<ImportPreview | ImportResult> {
	return invoke('backup_import', {
		srcPath,
		dryRun: opts.dryRun,
		passphrase: opts.passphrase ?? null,
	});
}

export async function backupList(): Promise<BackupSummary[]> {
	return invoke<BackupSummary[]>('backup_list');
}

export async function backupDelete(path: string): Promise<void> {
	return invoke('backup_delete', { path });
}

// ─── First-run wizard: system + agent detection ──────────────────────────────
//
// Three commands the onboarding wizard calls once on mount. All return rich
// JSON so the wizard can render without a second round-trip. Field names
// stay snake_case to match the Rust serde output verbatim — easier to
// audit than a hand-mapped camelCase translation.

export type CheckLevel = 'pass' | 'warn' | 'fail';

export interface SystemCheck {
	id: string;
	level: CheckLevel;
	message: string;
	fix_hint: string | null;
}

export interface SystemReport {
	os: string;
	arch: string;
	disk_free_gb: number;
	app_data_dir: string;
	app_data_writable: boolean;
	vault_key_present: boolean;
	claude_projects_dir_present: boolean;
	checks: SystemCheck[];
}

export interface AgentCapabilities {
	streaming: boolean;
	tool_use: boolean;
	thinking: boolean;
	artifacts: boolean;
	mcp: boolean;
	session_resume: boolean;
}

export interface DetectedAgent {
	id: string;
	display: string;
	executable_path: string;
	version: string | null;
	/** null = unknown / not probed; true = authed; false = not authed. */
	authed: boolean | null;
	/** Human-readable hint when `authed === false` or probe was inconclusive. */
	auth_hint: string | null;
	capabilities: AgentCapabilities;
}

export interface AgentConfigInventory {
	root_path: string;
	config_dir_present: boolean;
	agent_count: number;
	skill_count: number;
	command_count: number;
	mcp_server_count: number;
	project_count: number;
}

export async function detectSystem(): Promise<SystemReport> {
	return invoke<SystemReport>('detect_system');
}

export async function detectAgents(): Promise<DetectedAgent[]> {
	return invoke<DetectedAgent[]>('detect_agents');
}

export async function detectAgent(agentId: string): Promise<DetectedAgent | null> {
	return invoke<DetectedAgent | null>('detect_agent', { agentId });
}

export async function detectAgentConfig(
	agentId: string,
	rootPath: string
): Promise<AgentConfigInventory> {
	return invoke<AgentConfigInventory>('detect_agent_config', {
		agentId,
		rootPath,
	});
}

// Light-weight scan of `~/.claude/projects/` so the roots step can seed
// suggestions. The slug→path decode is best-effort (Claude Code slugifies
// `/` → `-`, which is lossy for components with internal hyphens) — the
// wizard treats results as suggestions, not source-of-truth.
export interface ClaudeProjectEntry {
	slug: string;
	/** Best-effort decoded path. When `path_verified` is false this is
	 *  the naive `s/-/\//g` decode — the wizard should show it as a
	 *  guess the user can edit before adding. */
	path: string;
	display_path: string;
	session_count: number;
	last_modified_ms: number;
	/** True iff the Rust side could `metadata()` the decoded path. */
	path_verified: boolean;
}

export async function listClaudeProjects(): Promise<ClaudeProjectEntry[]> {
	return invoke<ClaudeProjectEntry[]>('list_claude_projects');
}

export async function listAgentProjects(agentId: string): Promise<ClaudeProjectEntry[]> {
	return invoke<ClaudeProjectEntry[]>('list_agent_projects', { agentId });
}

// Agent-config scaffolder (Phase 6). Lays down a starter set of
// agents/skills/commands for the given provider into `<rootPath>/.claude/`
// (or the provider's equivalent). `mode` mirrors the Rust ScaffoldMode:
//   - 'augment'        (default) — write only files that don't exist
//   - 'replace'                  — overwrite everything
//   - 'skip_conflicts'           — like augment but the response lists
//                                  every conflict-skipped path
export type ScaffoldAgentConfigMode = 'replace' | 'augment' | 'skip_conflicts';

export interface ScaffoldAgentConfigFileEntry {
	path: string;
	reason: string;
}

export interface ScaffoldAgentConfigResult {
	ok: boolean;
	files_written: number;
	message: string;
	written: string[];
	skipped: ScaffoldAgentConfigFileEntry[];
	errors: ScaffoldAgentConfigFileEntry[];
}

export async function scaffoldAgentConfig(
	provider: string,
	rootPath: string,
	profile: string,
	mode: ScaffoldAgentConfigMode = 'augment'
): Promise<ScaffoldAgentConfigResult> {
	return invoke<ScaffoldAgentConfigResult>('scaffold_agent_config', {
		provider,
		rootPath,
		profile,
		mode,
	});
}

// ─── Projects (Phase 0 — first-class) ────────────────────────────────────────
//
// Projects are top-level scoping containers for sessions, pkgs, layout state,
// memory, etc. Migration 0015 introduces the `projects` + `project_settings`
// tables; a Default project ships in the seed and cannot be archived. The
// active project id is mirrored in `settings_kv` under `shell.activeProjectId`
// (Rust-owned). Switches emit a Tauri event `projects:active-changed` so
// project-scoped queries can invalidate.
//
// Wire format mirrors `src-tauri/src/commands/projects.rs` exactly: snake_case
// over the JSON wire because the Rust struct is serde-default. Keep these
// types snake_case in TS too — there is no boundary translation.

export interface Project {
	id: string;
	display_name: string;
	root_path: string | null;
	icon: string | null;
	color: string | null;
	description: string | null;
	position: number;
	is_default: boolean;
	created_at: number;
	archived_at: number | null;
}

export interface ProjectCreateArgs {
	id: string;
	display_name: string;
	root_path?: string | null;
	icon?: string | null;
	color?: string | null;
	description?: string | null;
}

export interface ProjectPatch {
	display_name?: string | null;
	root_path?: string | null;
	icon?: string | null;
	color?: string | null;
	description?: string | null;
	position?: number | null;
}

export async function projectCreate(args: ProjectCreateArgs): Promise<Project> {
	return invoke<Project>('project_create', {
		id: args.id,
		displayName: args.display_name,
		rootPath: args.root_path ?? null,
		icon: args.icon ?? null,
		color: args.color ?? null,
		description: args.description ?? null,
	});
}

export async function projectUpdate(id: string, patch: ProjectPatch): Promise<Project> {
	return invoke<Project>('project_update', { id, patch });
}

export async function projectList(includeArchived = false): Promise<Project[]> {
	return invoke<Project[]>('project_list', { includeArchived });
}

export async function projectArchive(id: string): Promise<void> {
	return invoke('project_archive', { id });
}

export async function projectSetActive(id: string): Promise<void> {
	return invoke('project_set_active', { id });
}

export async function projectGetActive(): Promise<Project> {
	return invoke<Project>('project_get_active');
}

export interface ProjectInventory {
	root_path: string | null;
	has_claude_dir: boolean;
	skills: number;
	commands: number;
	mcp: number;
}

/** Filesystem inventory of a project's Claude assets. Counts `.md` skills +
 *  `SKILL.md` folders under `<root>/.claude/skills/`, `.md` files under
 *  `<root>/.claude/commands/`, and `mcpServers` keys in `<root>/.mcp.json`.
 *  Pure read; safe to call any time. Returns zeroed counts when `root_path`
 *  is null or the directory doesn't exist. */
export async function projectInventory(rootPath: string | null): Promise<ProjectInventory> {
	return invoke<ProjectInventory>('project_inventory', { rootPath });
}

export interface ProjectSkill {
	slug: string;
	name: string | null;
	description: string | null;
	/** `'project'` — from `<root>/.claude/skills/`.
	 *  `'user'`    — from `~/.claude/skills/`. Project entries shadow user
	 *  entries with the same slug (project wins, matching claude code's
	 *  resolution order). */
	source: 'project' | 'user';
}

/** Enumerate skills available to a project. Walks `<root>/.claude/skills/`
 *  for `*.md` (single-file format) and folders containing `SKILL.md` (skill-
 *  folder format), and optionally `~/.claude/skills/` for user-global ones.
 *  Pure filesystem read with light frontmatter parsing — used by the
 *  artifact creation wizard's step-3 checklist. */
export async function projectSkillsList(
	rootPath: string | null,
	includeUserGlobal = true
): Promise<ProjectSkill[]> {
	return invoke<ProjectSkill[]>('project_skills_list', {
		rootPath,
		includeUserGlobal,
	});
}

/** Scaffold a minimal `<root>/.claude/{skills,commands}/` + `CLAUDE.md` stub
 *  in the given folder. Idempotent — leaves existing files alone. Used by
 *  Settings → Projects "Initialise new" so a chosen empty folder becomes a
 *  valid Claude project root before `projectCreate`. */
export async function projectScaffoldClaude(rootPath: string): Promise<void> {
	return invoke('project_scaffold_claude', { rootPath });
}

export interface ArtifactRow {
	path: string;
	name: string;
	/** Archetype slug from `manifest.notes.kind`. */
	kind: string | null;
	version: string | null;
	starred: boolean;
	has_manifest: boolean;
	/** ms since epoch */
	modified_at: number;
	size_bytes: number;
}

/** Recursive `.html` walk under a project root. Each result carries the
 *  manifest preview (name / kind / version / starred) when one is present,
 *  or `has_manifest: false` with the basename as the name. Bounded at
 *  5000 files / 2MB per parse on the Rust side; skips churn dirs
 *  (.git, node_modules, target, dist, etc.). Returns `[]` for null root. */
export async function projectArtifactsWalk(rootPath: string | null): Promise<ArtifactRow[]> {
	return invoke<ArtifactRow[]>('project_artifacts_walk', { rootPath });
}

/** Subscribe to project-switch events. The Rust side emits `projects:active-changed`
 *  with `{ id }` payload whenever `project_set_active` succeeds. Consumers
 *  typically just call `queryClient.invalidateQueries({ queryKey: ['project-scoped'] })`.
 *
 *  Tauri 2.11+ enforces event-name validation: dots aren't allowed in names
 *  (`IllegalEventName`), so the canonical separator here is `:`. */
export async function projectListenActiveChanged(
	callback: (payload: { id: string }) => void
): Promise<UnlistenFn> {
	return listen<{ id: string }>('projects:active-changed', (e) => callback(e.payload));
}

// ─── Atelier skill files (WP-16b / WP-10) ─────────────────────────────────────
//
// Generic reader for per-project Atelier skill config living under
// `<project_root>/.atelier/<skill>/<file>`. Returns the raw file contents, or
// `null` when the file is absent, the project has no root, or a segment is
// unsafe. The caller parses/validates; an absent or malformed value causes the
// consuming pkg to fall back to its static defaults.
//
// The `.atelier` prefix is hard-coded on the Rust side and both segments are
// validated against path traversal, so callers cannot read outside the
// project's `.atelier/` directory.

/** Read `<projectRoot>/.atelier/<skill>/<file>`.
 *  Returns the raw contents on success, `null` when absent or on IO error.
 *  Pass `null` for projects with no root configured — Rust returns `null`
 *  immediately without a filesystem access. */
export async function atelierFileRead(
	projectRoot: string | null,
	skill: string,
	file: string
): Promise<string | null> {
	return invoke<string | null>('atelier_file_read', { projectRoot, skill, file });
}

/** Read the Tasks pkg roster (`.atelier/skill-tasks/roster.json`) — a thin
 *  caller of the generic {@link atelierFileRead}. Absent/malformed → the Tasks
 *  pkg falls back to its static defaults (see `resolveRoster` in `assignees.js`). */
export async function skillRosterRead(projectRoot: string | null): Promise<string | null> {
	return atelierFileRead(projectRoot, 'skill-tasks', 'roster.json');
}

/** Atomically write `<projectRoot>/.atelier/<skill>/<file>` with `content`
 *  (WP-18b R12/R13 — the write-path sibling of {@link atelierFileRead}). The
 *  Rust command creates parent dirs, writes to a sibling temp file, then
 *  renames over the target so a concurrent read never sees a half-written file.
 *
 *  Unlike the reader (which swallows every error into `null` so consumers fall
 *  back to defaults), the write **rejects** on failure — the setup surface
 *  must know whether the confirm-write actually landed. Resolves to the written
 *  absolute path on success.
 *
 *  The command is intentionally generic: it writes whatever bytes it is given.
 *  Envelope shape (`skill` / `template_version` / `configured_at` / `settings`)
 *  and the `configured_at` stamp are the caller's responsibility, mirroring how
 *  the reader leaves parsing to the FE. */
export async function atelierFileWrite(
	projectRoot: string | null,
	skill: string,
	file: string,
	content: string
): Promise<string> {
	return invoke<string>('atelier_file_write', { projectRoot, skill, file, content });
}

// ─── Bun runtime fetch (B+A hybrid) ───────────────────────────────────────────
//
// The shell no longer bundles bun; on a fresh install it's fetched after the
// window is interactive (sha-pin-verified). The Rust side narrates progress on
// `runtime://bun`; the floating chip in `src/shell/runtime-bun-chip.tsx`
// listens and shows "Fetching runtime… NN%" with a Retry on error.

/** Wire shape emitted on `runtime://bun` by `runtime::ensure_bun`. */
export type RuntimeBunEvent =
	| { state: 'checking' }
	| { state: 'downloading'; pct: number }
	| { state: 'verifying' }
	| { state: 'ready' }
	| { state: 'error'; msg: string };

/** Subscribe to bun-fetch progress. */
export async function runtimeBunListen(
	callback: (payload: RuntimeBunEvent) => void
): Promise<UnlistenFn> {
	return listen<RuntimeBunEvent>('runtime://bun', (e) => callback(e.payload));
}

/** Re-trigger the bun fetch after a failure (chip Retry button). No-op if a
 *  fetch is already in flight or bun is already resolved. */
export async function runtimeRetryBunFetch(): Promise<void> {
	return invoke('runtime_retry_bun_fetch');
}

// ─── Phase 0.5 background-execution spike (debug-only) ────────────────────────
//
// Pairs with src-tauri/src/commands/bg_spike.rs. The Rust side is gated on
// cfg(debug_assertions); the FE side is gated on import.meta.env.DEV by
// the install helper in src/main.tsx (window.__bgSpikeInstall). Delete both
// once Phase 0.5 is signed off — these aren't a permanent surface.

export interface BgSpikeReport {
	intendedCount: number;
	completedCount: number;
	timeoutCount: number;
	durationMs: number;
	p50Us: number;
	p95Us: number;
	p99Us: number;
	maxUs: number;
	sampleUs: number[];
	webviewLabel: string;
}

export async function bgSpikeRun(
	durationMs: number,
	intervalMs: number,
	perPingTimeoutMs: number
): Promise<BgSpikeReport> {
	return invoke<BgSpikeReport>('bg_spike_run', {
		durationMs,
		intervalMs,
		perPingTimeoutMs,
	});
}

// ─── Artifact comments (artifact-grid pin overlay) ───────────────────────
//
// See plans/shell/2026-05-16-artifact-grid-brainstorm.md. Schema in migration
// 0022. Routing (terminal claude vs side-pane terminal) is handled by a separate
// dispatcher (Phase 4); these are pure persistence + lifecycle calls.

export type CommentStatus = 'open' | 'in_progress' | 'resolved' | 'stale';
export type CommentSink = 'terminal' | 'sidepane' | 'both';

export interface Comment {
	id: number;
	artifactPath: string;
	selector: string;
	text: string;
	screenshotPath: string | null;
	status: CommentStatus;
	positionX: number | null;
	positionY: number | null;
	threadId: string | null;
	openingSessionId: string | null;
	sink: CommentSink | null;
	createdAt: number;
	acknowledgedAt: number | null;
	resolvedAt: number | null;
}

export async function commentCreate(args: {
	artifactPath: string;
	selector: string;
	text: string;
	screenshotPath?: string | null;
	positionX?: number | null;
	positionY?: number | null;
}): Promise<Comment> {
	return invoke<Comment>('comment_create', {
		artifactPath: args.artifactPath,
		selector: args.selector,
		text: args.text,
		screenshotPath: args.screenshotPath ?? null,
		positionX: args.positionX ?? null,
		positionY: args.positionY ?? null,
	});
}

export async function commentGet(id: number): Promise<Comment> {
	return invoke<Comment>('comment_get', { id });
}

/** List pin comments. Pass `artifactPath` to scope to one artifact (cell
 *  render path), or omit for the cross-folder inbox view. Resolved pins are
 *  hidden by default. */
export async function commentList(args?: {
	artifactPath?: string | null;
	includeResolved?: boolean;
}): Promise<Comment[]> {
	return invoke<Comment[]>('comment_list', {
		artifactPath: args?.artifactPath ?? null,
		includeResolved: args?.includeResolved ?? false,
	});
}

/** Record which sink the dispatcher chose for this pin. Stamps `sink` plus
 *  `threadId` / `openingSessionId` audit fields. Idempotent — re-routing
 *  the same pin overwrites `sink` but COALESCEs the session ids so the
 *  earliest values stay. */
export async function commentRecordRouting(args: {
	id: number;
	sink: CommentSink;
	threadId?: string | null;
	openingSessionId?: string | null;
}): Promise<Comment> {
	return invoke<Comment>('comment_record_routing', {
		id: args.id,
		sink: args.sink,
		threadId: args.threadId ?? null,
		openingSessionId: args.openingSessionId ?? null,
	});
}

/** Set the pin's status. Agent transitions: open→in_progress (via
 *  `pin_acknowledge`) and any→resolved (via `pin_resolve`). User can also
 *  resolve manually from the grid UI. Timestamp fields are stamped on the
 *  first transition into each state and never overwritten. */
export async function commentSetStatus(id: number, status: CommentStatus): Promise<Comment> {
	return invoke<Comment>('comment_set_status', { id, status });
}

export async function commentDelete(id: number): Promise<void> {
	return invoke('comment_delete', { id });
}

/** Persist a base64-encoded PNG (from `captureToPng` on the FE) to disk
 *  under `$app_data_dir/pin-screenshots/<uuid>.png` and return the absolute
 *  path. Used by the pin composer to materialise an element screenshot
 *  before stamping the path on `commentCreate`. */
export async function pinScreenshotWrite(base64Png: string): Promise<string> {
	return invoke<string>('pin_screenshot_write', { base64Png });
}

/** Routing sink.
 *  - `terminal` writes a one-line nudge to the active claude PTY's stdin;
 *    claude pulls the full payload via `mcp-iyke.read_pin`.
 *  - `chi` spawns a headless one-off agent run seeded with the pin prompt
 *    and returns its `runId`. Spends tokens, so it is never auto-selected.
 *  - `clipboard` returns the rendered prompt in `clipboardText` for the FE
 *    to copy. Zero side effects, always available — the auto fallback.
 *
 *  The `sidepane` and `both` sinks were removed with the chat surface: they
 *  emitted a `pin://routed` event whose only consumer was the side-pane
 *  Chat composer, so after WP-05 they silently swallowed pins. */
export type RouteSink = 'terminal' | 'chi' | 'clipboard';

export interface RouteResult {
	/** Sink actually used, which may differ from the requested one — a
	 *  `terminal` route whose PTY died degrades to `clipboard`. */
	sink: RouteSink | null;
	/** PTY id used when the terminal branch fired. */
	ptyId: string | null;
	/** Foreground command on that PTY at routing time (e.g. `"claude"`). */
	ptyForeground: string | null;
	/** Chi run id, when the `chi` sink fired. */
	runId: string | null;
	/** Rendered prompt to copy, when the `clipboard` sink fired. */
	clipboardText: string | null;
	comment: Comment;
}

/** Dispatch a pin to its routing sink. Auto-detects when `overrideSink` is
 *  omitted: most-recently-active claude PTY wins, else clipboard. ⌥-click on
 *  the pin in the grid passes an explicit override.
 *
 *  `preferredPtyId` lets the caller pin delivery to a specific terminal
 *  (typically the most-recently-focused tab) so two concurrent claude PTYs
 *  don't race for the route. The dispatcher honours the hint only if that
 *  PTY's foreground is still claude; otherwise it falls back to the snapshot
 *  scan.
 *
 *  Callers should prefer `routePin` in `@/lib/artifact/route-pin`, which also
 *  performs the clipboard write when the result asks for one. */
export async function commentRoute(args: {
	id: number;
	overrideSink?: RouteSink;
	preferredPtyId?: string | null;
}): Promise<RouteResult> {
	const raw = await invoke<{
		sink: string | null;
		pty_id: string | null;
		pty_foreground: string | null;
		run_id: string | null;
		clipboard_text: string | null;
		comment: Comment;
	}>('comment_route', {
		id: args.id,
		overrideSink: args.overrideSink ?? null,
		preferredPtyId: args.preferredPtyId ?? null,
	});
	return {
		sink: (raw.sink as RouteSink | null) ?? null,
		ptyId: raw.pty_id,
		ptyForeground: raw.pty_foreground,
		runId: raw.run_id,
		clipboardText: raw.clipboard_text,
		comment: raw.comment,
	};
}

// ─── Approve-gate run-then-pause seam (pa_action_drafts) ──────────────────────
//
// Wrappers + event listeners for the approve-gate seam (WP-4). Rust side:
// src-tauri/src/commands/pa_actions.rs (WP-3); scope:
// plans/atelier/10-approve-gate-seam.md. The list returns opaque rows — parse
// `payloadJson`/`editedJson` and derive the PausedDraft view-model with
// `fromDraftItem` from @ikenga/contract.

/** One draft row as stored in pa_action_drafts. `payloadJson` is a DraftItem +
 *  ApproveGateMeta; `editedJson` holds operator subject/body overrides.
 *  The 0051 columns (claimedAt … deliveryCheckedAt) are written by the external
 *  mutation worker; the shell surfaces them for failure-surfacing (WP-12 / G-09). */
export interface PaActionDraftRow {
	id: string;
	batchId: string;
	actionId: string;
	/** awaiting | edited | committed | sending | sent | failed | rejected */
	status: string;
	channel: string;
	payloadJson: string;
	editedJson: string | null;
	scheduledAt: string | null;
	createdAt: string;
	committedAt: string | null;
	sentAt: string | null;
	// ── 0051 mutation-worker columns ──────────────────────────────────────────
	claimedAt: string | null;
	/** Number of send attempts so far; 0 before the worker ever claimed the row. */
	attempts: number;
	lastAttemptAt: string | null;
	/** Last error message written by the worker on failure. */
	errorText: string | null;
	/** Provider message/campaign/post id written on success. */
	externalId: string | null;
	/** null | accepted | delivered | bounced | complained | errored */
	deliveryStatus: string | null;
	deliveryCheckedAt: string | null;
}

/** One draft in a pause batch. `payload` (DraftItem + ApproveGateMeta) is stored
 *  verbatim and parsed FE-side. */
export interface PaPauseDraftInput {
	id: string;
	channel: string;
	scheduledAt?: string | null;
	payload: unknown;
}

export interface PaActionPausedEvent {
	batchId: string;
	count: number;
}
export interface PaActionCommittedEvent {
	draftId: string;
	channel: string;
	payloadJson: string;
	editedJson: string | null;
}
export interface PaActionRejectedEvent {
	draftId: string;
}

/** List drafts in the gate. Defaults to the active set (awaiting/edited/
 *  committed); pass a status to filter (e.g. 'sent', 'rejected'). */
export async function paActionsList(status?: string): Promise<PaActionDraftRow[]> {
	return invoke<PaActionDraftRow[]>('pa_actions_list', { status: status ?? null });
}

/** Pause a batch of drafts — the producer hand-off. Inserts one awaiting row per
 *  draft and emits `pa-action-paused`. Returns the row count. */
export async function paActionsPause(
	batchId: string,
	actionId: string,
	drafts: PaPauseDraftInput[]
): Promise<number> {
	return invoke<number>('pa_actions_pause', { batchId, actionId, drafts });
}

/** Persist operator inline edits ({ subject?, body? }) onto a draft. */
export async function paActionsUpdate(
	draftId: string,
	patch: { subject?: string; body?: string }
): Promise<void> {
	return invoke('pa_actions_update', { draftId, patch });
}

/** Commit a draft (post-undo). Flips it to committed + emits
 *  `pa-action-committed` for the external mutation worker. The shell never sends. */
export async function paActionsCommit(draftId: string): Promise<void> {
	return invoke('pa_actions_commit', { draftId });
}

/** Reject a draft. Flips it to rejected + emits `pa-action-rejected`. */
export async function paActionsReject(draftId: string): Promise<void> {
	return invoke('pa_actions_reject', { draftId });
}

/** Re-queue a failed draft for another send attempt (WP-12 / G-09).
 *  Flips failed → committed and wakes the mutation worker. Only operates on
 *  `failed` rows — returns an error string if the row is in any other state. */
export async function paActionsRetry(draftId: string): Promise<void> {
	return invoke('pa_actions_retry', { draftId });
}

/** Fires when an approve-aware action pauses a batch — mount the gate. */
export function onPaActionPaused(callback: (e: PaActionPausedEvent) => void): Promise<UnlistenFn> {
	return listen<PaActionPausedEvent>('pa-action-paused', (e) => callback(e.payload));
}

/** Fires after commit — the external mutation worker performs the real send. */
export function onPaActionCommitted(
	callback: (e: PaActionCommittedEvent) => void
): Promise<UnlistenFn> {
	return listen<PaActionCommittedEvent>('pa-action-committed', (e) => callback(e.payload));
}

/** Fires when a draft is rejected. */
export function onPaActionRejected(
	callback: (e: PaActionRejectedEvent) => void
): Promise<UnlistenFn> {
	return listen<PaActionRejectedEvent>('pa-action-rejected', (e) => callback(e.payload));
}

// ── multi-window substrate (plans/multi-window WP-03) ────────────────────────
// Mirrors `commands/window.rs`. The descriptor type is the `G-WINDOW-MODEL`
// contract from `@ikenga/contract` (WP-02).

/** Spawn a labeled window from a descriptor; resolves to the window label. */
export async function spawnWindow(descriptor: WindowDescriptor): Promise<string> {
	return invoke<string>('window_spawn', { descriptor });
}

/** Close a spawned window by label (`main` is refused). */
export async function closeWindow(label: string): Promise<void> {
	return invoke('window_close', { label });
}

/** List descriptors of all currently-spawned windows. */
export async function listWindows(): Promise<WindowDescriptor[]> {
	return invoke<WindowDescriptor[]>('window_list');
}

// ── Chi-first agent surface (plans/2026-08-08-ikenga-chi-first WP-01) ─────────

export type ChiRunStatus =
	| 'queued'
	| 'running'
	| 'awaiting_auth'
	| 'done'
	| 'failed'
	| 'cancelled'
	| 'timed_out';

export interface ChiRunResult {
	run_id: string;
	status: ChiRunStatus;
	output?: string;
	output_truncated?: boolean;
	error?: string;
}

export interface ChiCacheRow {
	run_id: string;
	engine_id: string;
	external_id?: string;
	brief?: string;
	cwd?: string;
	model?: string;
	mode?: string;
	status: string;
	output_path?: string;
	output_truncated?: boolean;
	error?: string;
	artifacts?: unknown;
	parent_id?: string;
	owner: string;
	terminal_session_id?: string;
	started_at?: string;
	ended_at?: string;
	last_seen_at?: string;
	expires_at?: string;
}

export interface ChiRunOpts extends Record<string, unknown> {
	engineId: string;
	prompt: string;
	cwd?: string;
	model?: string;
	mode?: string;
	timeoutSeconds?: number;
	parentId?: string;
	resumeSessionId?: string;
}

/** Start a new Chi run. The engine child is spawned in WP-02; WP-01 mints
 *  a run id and persists a cache row. */
export async function chiRun(opts: ChiRunOpts): Promise<ChiRunResult> {
	return invoke<ChiRunResult>('chi_run', opts);
}

/** Resume an existing Chi run by its Ikenga run_id. */
export async function chiResume(runId: string, prompt: string): Promise<ChiRunResult> {
	return invoke<ChiRunResult>('chi_resume', { runId, prompt });
}

/** Read the status of a Chi run from the cache. */
export async function chiStatus(runId: string): Promise<ChiRunResult> {
	return invoke<ChiRunResult>('chi_status', { runId });
}

/** List cached Chi runs, optionally filtered by engine. */
export async function chiList(
	engineId?: string | null,
	limit?: number | null
): Promise<ChiCacheRow[]> {
	return invoke<ChiCacheRow[]>('chi_list', { engineId: engineId ?? null, limit: limit ?? null });
}

/** Cancel a Chi run. */
export async function chiCancel(runId: string): Promise<ChiRunResult> {
	return invoke<ChiRunResult>('chi_cancel', { runId });
}
