// Iyke wire types. These mirror the Rust handlers in
// src-tauri/src/iyke/handlers.rs — keep in sync. The schema_version field
// is the contract: bump it on the Rust side when the response shape
// changes in a non-additive way.

export interface IykeEndpoint {
	url: string;
	token: string;
	port: number;
	/** Absolute path to the `--settings` file Ikenga hands `claude` so its hooks
	 *  and statusline reach this bridge. `null` if it could not be written — the
	 *  terminal wrapper then omits `--settings` rather than launching a session
	 *  wired to a port that isn't listening. See `iyke::hook_settings` (Rust). */
	hooks_settings_path: string | null;
}

export interface IykeAppInfo {
	pid: number;
	started_at_unix_ms: number;
	identifier: string;
}

export interface IykeShellInfo {
	mode: string | null;
	route: string | null;
	/** Phase 12 PR-E. Null when the FE hasn't pushed yet. */
	panes: IykePanesPayload | null;
	sidebar_collapsed: boolean | null;
}

export interface IykeLeafSummary {
	id: string;
	focused: boolean;
	activeTabIdx: number;
	tabs: Array<{
		kind: string;
		title: string;
		terminalId?: string;
		ptyId?: string;
	}>;
}

/**
 * Pane-tree wire shape. `tree` is the recursive PaneNode the FE owns —
 * Rust treats it as opaque JSON. `leaves` is a flat DFS-ordered list
 * for clients that don't want to walk the tree (most CLI/MCP cases).
 */
export interface IykePanesPayload {
	leaves: IykeLeafSummary[];
	tree: unknown;
}

export interface IykeTerminalInfo {
	terminal_id: string;
	pty_id: string;
	title: string;
	label: string | null;
	cwd: string;
	argv: string[];
	status: 'running' | 'exited';
	pid: number | null;
	foreground_command: { pid: number; name: string; args: string[] } | null;
	created_at: number;
	exited_at: number | null;
	exit_code: number | null;
	output_start_offset: number;
	output_end_offset: number;
	owner_agent_id: string | null;
	lease_expires_at: number | null;
	mounted: boolean;
	focused: boolean;
	pane_ids: string[];
	window_labels: string[];
}

export interface IykeWindowInfo {
	label: string;
	kind: 'primary' | 'single-surface' | 'pane-set' | 'workspace';
	surface_set: string[];
	project_id: string | null;
	layout_key: string;
	panes: IykePanesPayload | null;
}

export interface IykeStateResponse {
	schema_version: number;
	app: IykeAppInfo;
	shell: IykeShellInfo;
	terminals: IykeTerminalInfo[];
	windows: IykeWindowInfo[];
}
