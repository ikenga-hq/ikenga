// Browser-mode Tauri host mock for the Playwright frame harness (WP-19).
//
// The shell talks to its Rust host through exactly one seam:
// `window.__TAURI_INTERNALS__.invoke` (every `@/lib/tauri-cmd` wrapper goes
// `invoke()` → `TauriTransport` → `@tauri-apps/api/core` → that function, and
// `listen()` goes through `plugin:event|listen` on the same path). So instead
// of aliasing `@/lib/tauri-cmd` in the bundler, we install a fake
// `__TAURI_INTERNALS__` before any app script runs. `isTauri()` then returns
// true, the real `TauriTransport` is used, and every command the frame issues
// lands in `handle()` below with a canned response. Product code and the
// vitest config stay untouched.
//
// Responses are keyed by Tauri command name (the string passed to `invoke`,
// e.g. `project_list`, `activity_pins_list`, `pkg_kernel_status`,
// `pty_spawn`). A spec can override any of them per test:
//
//   await installTauriMock(page, { responses: { activity_pins_list: [myPin] } });
//
// Unknown commands resolve to `null` and are recorded, so a spec can assert on
// what the frame asked for (`window.__IKENGA_E2E__.calls`) and push host
// events into the page (`window.__IKENGA_E2E__.emit('projects:active-changed',
// {...})`).

import type { Page } from '@playwright/test';

/** Serialisable command → response table. A value of the shape
 *  `{ __error: 'msg' }` makes the command reject with that message. */
export type MockResponses = Record<string, unknown>;

export interface TauriMockOptions {
	/** Merged over `DEFAULT_RESPONSES`. */
	responses?: MockResponses;
	/** Platform the fake `os` / path plugins report. Defaults to `linux` so the
	 *  frame renders the same chrome on every CI host. */
	platform?: 'linux' | 'macos' | 'windows';
	/** Let requests leave the Vite origin (fonts, CDNs). Off by default. */
	allowExternal?: boolean;
}

export interface E2ECall {
	cmd: string;
	args: unknown;
}

const NOW = 1_760_000_000_000;

export const MOCK_PROJECTS = [
	{
		id: 'default',
		display_name: 'Default',
		root_path: null,
		icon: null,
		color: null,
		description: null,
		position: 0,
		is_default: true,
		created_at: NOW,
		archived_at: null,
	},
	{
		id: 'label-ops',
		display_name: 'Label Ops',
		root_path: '/home/e2e/label-ops',
		icon: null,
		color: null,
		description: null,
		position: 1,
		is_default: false,
		created_at: NOW + 1,
		archived_at: null,
	},
];

export const MOCK_SECTIONS = [
	{
		id: 'e2e-work',
		label: 'Work',
		iconLucide: null,
		iconEmoji: null,
		sortOrder: 0,
		createdAt: '2026-01-01T00:00:00Z',
	},
];

export const MOCK_PINS = [
	{
		id: 'pin-todos',
		kind: 'route',
		target: '/todos',
		label: 'Pinned Todos',
		iconLucide: 'square-check',
		iconEmoji: null,
		sectionId: 'e2e-work',
		sortOrder: 0,
		createdAt: '2026-01-01T00:00:00Z',
		manifestId: null,
		lastOpenedAt: null,
	},
];

/** Kernel snapshot: one app pkg contributing one rail entry. */
export const MOCK_KERNEL_STATUS = {
	api_version: 1,
	installed: [
		{
			id: 'com.e2e.demo',
			version: '0.0.1',
			ikenga_api: '^1',
			install_path: '/home/e2e/pkgs/com.e2e.demo',
			enabled: true,
			installed_at: NOW,
			compatible: true,
			source: 'local',
			project_id: null,
		},
	],
	registries: {
		activity_bar: {
			entries: [
				{
					pkg_id: 'com.e2e.demo',
					pkg_name: 'Demo Pkg',
					id: 'demo',
					label: 'Demo Pkg',
					icon: 'Box',
					section: null,
					route: '/pkg/com.e2e.demo',
					nav: [{ id: 'demo', label: 'Demo Pkg', icon: 'Box', route: '/pkg/com.e2e.demo' }],
					badge: null,
					parked: false,
				},
			],
		},
		sidecars: { sidecars: [] },
	},
};

const ONBOARDING_STEPS = [
	'welcome',
	'agent',
	'roots',
	'packages',
	'connectors',
	'scaffolding',
	'appearance',
	'summary',
];

/** A finished first-run, as `settings_kv['onboarding.state']` stores it
 *  (`OnboardingState` in `src/lib/shell/shell-store.ts`). Without it the
 *  root route bounces every load to `/onboarding` instead of the workspace. */
export const COMPLETED_ONBOARDING = {
	version: 2,
	startedAt: NOW,
	completedAt: NOW,
	mode: 'first_run',
	activeIndex: ONBOARDING_STEPS.length - 1,
	steps: Object.fromEntries(
		ONBOARDING_STEPS.map((id) => [id, { status: 'completed', completedAt: NOW }])
	),
	selectedAgentId: null,
	loreGlossSeen: [],
};

/** settings_kv values are JSON strings. */
export const MOCK_SETTINGS: Record<string, string> = {
	'onboarding.state': JSON.stringify(COMPLETED_ONBOARDING),
	'user.name': JSON.stringify('E2E'),
	'updates.autoCheck': JSON.stringify(false),
};

/** Baseline host: the smallest set of answers that lets today's frame boot
 *  to an interactive workspace. Keep entries here generic — anything a single
 *  spec cares about belongs in that spec's `responses` override. */
export const DEFAULT_RESPONSES: MockResponses = {
	// Projects (Phase 0 projects-first-class)
	project_list: MOCK_PROJECTS,
	project_get_active: MOCK_PROJECTS[0],
	project_set_active: null,
	// Rail pins + sections
	activity_pins_list: MOCK_PINS,
	activity_sections_list: MOCK_SECTIONS,
	// Pkg kernel snapshot (rail pkg entries, sidebar pkg menus)
	pkg_kernel_status: MOCK_KERNEL_STATUS,
	pkg_list_installed: [],
	// PTY — the frame never needs a live pty to render; spawn hands back a
	// stable id so a terminal tab can mount without a host.
	pty_spawn: 'e2e-pty-1',
	pty_write: null,
	pty_resize: null,
	pty_kill: null,
	pty_list: [],
	terminal_list: [],
	pty_terminal_list: [],
	// FS / settings / SQLite passthrough
	fs_home: '/home/e2e',
	fs_roots_list: [],
	settings_get_all: MOCK_SETTINGS,
	settings_get: null,
	settings_set: null,
	db_query: [],
	db_execute: { rows_affected: 0, last_insert_id: null },
	// Host services the frame probes at boot
	supabase_config_get: null,
	window_list: [],
	list_all_skill_actions: [],
	iyke_set_shell: null,
	iyke_log_push: null,
	// Unroutable on purpose: the fixture refuses non-local hosts, so nothing
	// in the page can reach a real bridge.
	iyke_endpoint: { url: 'http://iyke.e2e.invalid', token: 'e2e', port: 0 },
	detect_system: {
		os: 'linux',
		arch: 'x86_64',
		disk_free_gb: 100,
		app_data_dir: '/home/e2e/.local/share/app.ikenga',
		app_data_writable: true,
		vault_key_present: true,
		claude_projects_dir_present: true,
		checks: [],
	},
	// Tauri core plugins the frame touches at boot
	'plugin:app|version': '0.0.0-e2e',
	'plugin:app|name': 'Ikenga',
	'plugin:app|tauri_version': '2.0.0',
	'plugin:path|resolve_directory': '/home/e2e',
	'plugin:path|resolve': '/home/e2e',
	'plugin:path|join': '/home/e2e',
	'plugin:sql|load': 'sqlite:e2e.db',
	'plugin:sql|select': [],
	'plugin:sql|execute': [0, null],
	'plugin:sql|close': true,
	'plugin:window|is_fullscreen': false,
	'plugin:window|is_maximized': false,
	'plugin:window|is_focused': true,
	'plugin:window|scale_factor': 1,
	'plugin:window|inner_size': { width: 1440, height: 900 },
	'plugin:window|outer_size': { width: 1440, height: 900 },
	'plugin:window|inner_position': { x: 0, y: 0 },
	'plugin:window|outer_position': { x: 0, y: 0 },
	'plugin:window|theme': 'dark',
	'plugin:webview|get_all_webviews': [],
	'plugin:window|get_all_windows': ['main'],
	'plugin:updater|check': null,
	'plugin:notification|is_permission_granted': false,
};

interface InitPayload {
	responses: MockResponses;
	platform: string;
}

/**
 * Runs in the page before any app script (via `page.addInitScript`). Must be
 * self-contained — Playwright serialises the function source, so nothing from
 * this module's scope is visible inside it except `payload`.
 */
function installInPage(payload: InitPayload): void {
	type Cb = (data: unknown) => void;
	const w = window as unknown as Record<string, any>;
	const responses = payload.responses;
	const calls: { cmd: string; args: unknown }[] = [];
	const unknown = new Set<string>();
	const callbacks = new Map<number, Cb>();
	const listeners = new Map<string, number[]>();
	let nextId = 1;

	function transformCallback(cb?: Cb, once = false): number {
		const id = nextId++;
		callbacks.set(id, (data) => {
			if (once) callbacks.delete(id);
			cb?.(data);
		});
		return id;
	}

	function emit(event: string, payloadData: unknown): void {
		for (const handlerId of listeners.get(event) ?? []) {
			callbacks.get(handlerId)?.({ event, id: handlerId, payload: payloadData });
		}
	}

	function handle(cmd: string, args: any): unknown {
		switch (cmd) {
			case 'plugin:event|listen': {
				const list = listeners.get(args.event) ?? [];
				list.push(args.handler);
				listeners.set(args.event, list);
				return args.handler;
			}
			case 'plugin:event|unlisten': {
				const list = listeners.get(args.event);
				if (list)
					listeners.set(
						args.event,
						list.filter((id) => id !== args.eventId)
					);
				return null;
			}
			case 'plugin:event|emit':
			case 'plugin:event|emit_to':
				emit(args.event, args.payload);
				return null;
		}
		if (Object.hasOwn(responses, cmd)) {
			const v = responses[cmd] as any;
			if (v && typeof v === 'object' && '__error' in v) throw new Error(String(v.__error));
			// Fresh copy per call so a consumer mutating a response can't leak
			// into the next caller.
			return v === undefined ? null : JSON.parse(JSON.stringify(v));
		}
		unknown.add(cmd);
		return null;
	}

	w.__TAURI_INTERNALS__ = {
		invoke: async (cmd: string, args?: unknown) => {
			calls.push({ cmd, args });
			return handle(cmd, args ?? {});
		},
		transformCallback,
		unregisterCallback: (id: number) => callbacks.delete(id),
		runCallback: (id: number, data: unknown) => callbacks.get(id)?.(data),
		callbacks,
		convertFileSrc: (p: string, protocol = 'asset') =>
			`${protocol}://localhost/${encodeURIComponent(p)}`,
		metadata: {
			currentWindow: { label: 'main' },
			currentWebview: { windowLabel: 'main', label: 'main' },
		},
		plugins: {
			path: {
				sep: payload.platform === 'windows' ? '\\' : '/',
				delimiter: payload.platform === 'windows' ? ';' : ':',
			},
		},
	};
	w.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
		unregisterListener: (_event: string, id: number) => callbacks.delete(id),
	};
	w.__TAURI_OS_PLUGIN_INTERNALS__ = {
		platform: payload.platform,
		os_type: payload.platform,
		family: payload.platform === 'windows' ? 'windows' : 'unix',
		version: '0.0.0',
		arch: 'x86_64',
		exe_extension: payload.platform === 'windows' ? 'exe' : '',
		eol: payload.platform === 'windows' ? '\r\n' : '\n',
	};
	w.__IKENGA_E2E__ = {
		calls,
		emit,
		unknownCommands: () => Array.from(unknown).sort(),
	};
}

/** Install the fake Tauri host into `page`. Call before `page.goto`. */
export async function installTauriMock(page: Page, opts: TauriMockOptions = {}): Promise<void> {
	const payload: InitPayload = {
		responses: { ...DEFAULT_RESPONSES, ...(opts.responses ?? {}) },
		platform: opts.platform ?? 'linux',
	};
	await page.addInitScript(installInPage, payload);
	if (opts.allowExternal !== true) {
		// The frame must render offline. index.html preconnects to Google Fonts
		// and a hanging font request holds the `load` event hostage on a
		// sandboxed runner, so everything that isn't the Vite server is refused
		// (the frame falls back to system fonts).
		await page.route(
			(url) => url.hostname !== '127.0.0.1' && url.hostname !== 'localhost',
			(route) => route.abort('blockedbyclient')
		);
	}
}

/** Commands the page has invoked so far, in order. */
export async function invokedCommands(page: Page): Promise<E2ECall[]> {
	return page.evaluate(() => (window as any).__IKENGA_E2E__.calls as E2ECall[]);
}

/** Commands the page invoked that had no canned response (resolved `null`). */
export async function unmockedCommands(page: Page): Promise<string[]> {
	return page.evaluate(() => (window as any).__IKENGA_E2E__.unknownCommands() as string[]);
}

/** Deliver a host event (what Rust `app.emit(...)` would send) to the page. */
export async function emitHostEvent(page: Page, event: string, payload: unknown): Promise<void> {
	await page.evaluate(([e, p]) => (window as any).__IKENGA_E2E__.emit(e, p), [
		event,
		payload,
	] as const);
}
