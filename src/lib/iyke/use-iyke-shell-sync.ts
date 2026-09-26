import { useEffect } from 'react';
import { getHomeSync } from '@/lib/home';
import { formatKeyLabel } from '@/lib/keymap/platform';
import { findLeaf, getLeafIdsInOrder } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import type { PaneId, PaneNode, PaneView } from '@/lib/panes/types';
import { type ActiveProject, useShellStore } from '@/lib/shell/shell-store';
import {
	type IykeActionMirror,
	type IykeKeymapEntry,
	type IykeMenuMirror,
	type IykeMenuMirrorItem,
	iykeActionsRequestDone,
	iykeSetActionsFrame,
	iykeSetFrame,
	listen,
} from '@/lib/tauri-cmd';
import { type TerminalTab, useTerminalStore } from '@/terminal/session-store';
import { formatTerminalTitle } from '@/terminal/terminal-title';

import { setShell } from './client';
import { getIframe, IFRAME_STATE_EVENT } from './iframe-registry';
import {
	ActionsValidationError,
	addKeybinding,
	type ActionsScope,
	type EffectiveAction,
	type EffectiveModel,
	type KeymapEntry,
	listKeymap,
	getEffectiveModel,
	resolveKeypress,
	saveUserAction,
	subscribeEffectiveModel,
	subscribeKeymap,
	type UserAction,
} from './keymap-bridge';

/**
 * Bridge between React shell state and the Iyke Rust mirror. Mounted
 * once inside `<Workspace />` — i.e. only at the global, post-auth
 * level, never inside a pane's memory router.
 *
 * Pushes the active sidebar mode + focused pane's route + a flat
 * leaves-and-tree pane snapshot. Failures are logged, not thrown —
 * Iyke isn't on the user's critical path.
 */
export function useIykeShellSync(): void {
	const activeMode = useShellStore((s) => s.activeMode);
	// Re-fire when focused pane changes or when the tree mutates (any
	// navigation inside a pane updates `root` immutably via the reducer).
	const focusedId = usePaneStore((s) => s.focusedId);
	const root = usePaneStore((s) => s.root);
	// Subscribed so a sidebar collapse/expand pushes immediately. Without
	// this in the dep list the mirror would only catch up on the next
	// unrelated mode/pane change, and `/iyke/state` would report a stale
	// value right after `/iyke/sidebar` — the exact window a caller polls.
	const sidebarCollapsed = useShellStore((s) => s.sidebarCollapsed);
	const terminalTabs = useTerminalStore((s) => s.tabs);

	useEffect(() => {
		pushShellState(activeMode, sidebarCollapsed, root, focusedId, terminalTabs);
	}, [activeMode, focusedId, root, sidebarCollapsed, terminalTabs]);

	// Re-push when a pkg iframe publishes state (selection etc.) so
	// `iyke state` reflects it without waiting for a pane-tree mutation.
	useEffect(() => {
		const onState = () => {
			const shell = useShellStore.getState();
			const panes = usePaneStore.getState();
			pushShellState(
				shell.activeMode,
				shell.sidebarCollapsed,
				panes.root,
				panes.focusedId,
				useTerminalStore.getState().tabs
			);
		};
		window.addEventListener(IFRAME_STATE_EVENT, onState);
		return () => window.removeEventListener(IFRAME_STATE_EVENT, onState);
	}, []);

	// WP-21: `shell.active_project` in `iyke state`. `activeProject` is
	// derived and reference-stable (G-STATE §2), so this fires only when the
	// id, root or extra roots actually change.
	const activeProject = useShellStore((s) => s.activeProject);
	useEffect(() => {
		pushActiveProject(activeProject);
	}, [activeProject]);

	// WP-21: `GET /iyke/keys`. Phase 1's keymap was defaults-only and never
	// changed at runtime, so a boot-only push was enough. Phase 6 overrides
	// (personal/project keybindings, package grants, trust changes) mutate
	// the effective keymap at runtime, so WP-62 re-sends it on every
	// `subscribeKeymap()` notification, not just once at mount.
	useEffect(() => {
		const push = () => {
			iykeSetFrame({ keymap: keymapPayload() }).catch((err) => {
				console.warn('[iyke] set_frame (keymap) failed:', err);
			});
		};
		push();
		return subscribeKeymap(push);
	}, []);

	// WP-62: `GET /iyke/actions` + `GET /iyke/menus/:id`. Pushes a projection
	// of the effective model (G-ACTIONS-API) on every `subscribeEffectiveModel`
	// notification — file edits, package install/uninstall/reload, project
	// switch — so the `iyke` surface never serves a merge older than what the
	// D-06 UI itself would show.
	useEffect(() => {
		const push = (model: EffectiveModel) => {
			iykeSetActionsFrame({
				actions: actionsMirrorPayload(model.actions),
				menus: menusMirrorPayload(model),
			}).catch((err) => {
				console.warn('[iyke] set_actions_frame failed:', err);
			});
		};
		push(getEffectiveModel());
		return subscribeEffectiveModel(push);
	}, []);

	// WP-62: the write/query round trips `src-tauri/src/iyke/actions_routes.rs`
	// emits for `POST /iyke/actions/set|import`, `POST /iyke/keys/set` and
	// `GET /iyke/keys/resolve`. Each listener calls the same G-ACTIONS-API
	// function the D-06 UI calls, then reports back through
	// `iyke_actions_request_done` so the CLI and the UI share one code path
	// into the one WP-50 validator.
	useEffect(() => {
		const unlistenPromises = [
			listen<ActionsSetRequestPayload>('iyke://actions-set-request', (e) => {
				void handleActionsSetRequest(e.payload);
			}),
			listen<ActionsImportRequestPayload>('iyke://actions-import-request', (e) => {
				void handleActionsImportRequest(e.payload);
			}),
			listen<KeysSetRequestPayload>('iyke://keys-set-request', (e) => {
				void handleKeysSetRequest(e.payload);
			}),
			listen<KeysResolveRequestPayload>('iyke://keys-resolve-request', (e) => {
				void handleKeysResolveRequest(e.payload);
			}),
		];
		return () => {
			for (const p of unlistenPromises) {
				p.then((unlisten) => unlisten()).catch(() => {});
			}
		};
	}, []);

	// WP-28: `GET /iyke/explorer/sections`. `explorerSections` is replaced
	// wholesale by reorder/collapse mutations, so the subscription fires
	// exactly on change; the wire shape is the store's
	// `ExplorerSectionState` field-for-field (G-STATE §1).
	const explorerSections = useShellStore((s) => s.explorerSections);
	useEffect(() => {
		iykeSetFrame({ explorerSections }).catch((err) => {
			console.warn('[iyke] set_frame (explorer_sections) failed:', err);
		});
	}, [explorerSections]);
}

function pushActiveProject(activeProject: ActiveProject): void {
	iykeSetFrame({
		activeProject: {
			id: activeProject.id,
			root_path: activeProject.root_path,
			extra_roots: [...activeProject.extra_roots],
		},
	}).catch((err) => {
		console.warn('[iyke] set_frame (active_project) failed:', err);
	});
}

/** The registry rows `GET /iyke/keys` serves: `{command, key, when, source}`
 *  plus the human label and the key hint resolved for this platform. */
export function keymapPayload(entries: KeymapEntry[] = listKeymap()): IykeKeymapEntry[] {
	return entries.map((e) => ({
		command: e.command,
		key: e.key,
		when: e.when,
		source: e.source,
		label: e.label,
		key_label: formatKeyLabel(e.key),
		...(e.platformOnly ? { platform_only: e.platformOnly } : {}),
	}));
}

// ─── WP-62: `iyke` actions / menus / keys mirror + write round trips ────────

/** The rows `GET /iyke/actions` serves — a flattened projection of
 *  `EffectiveAction[]` (G-ACTIONS-API). Opaque to Rust beyond this shape
 *  (`IykeActionMirror`, `src/lib/tauri-cmd.ts`). */
export function actionsMirrorPayload(actions: EffectiveAction[]): IykeActionMirror[] {
	return actions.map((a) => ({
		id: a.id,
		name: a.name,
		...(a.icon ? { icon: a.icon } : {}),
		description: a.description,
		source: a.source,
		run_kind: a.run.kind,
		placements: a.placements.map((p) => p.at),
		locked: a.locked,
		hosted: a.hosted,
		danger: a.danger,
		...(a.pkgId ? { pkg_id: a.pkgId } : {}),
	}));
}

/** The rows `GET /iyke/menus/:id` serves — every menu id the effective
 *  model currently knows, keyed the same way `EffectiveMenus.get()` is. */
export function menusMirrorPayload(model: EffectiveModel): Record<string, IykeMenuMirror> {
	const out: Record<string, IykeMenuMirror> = {};
	for (const id of model.menus.ids) {
		const menu = model.menus.get(id);
		if (!menu) continue;
		const items: IykeMenuMirrorItem[] = menu.items.map((item) =>
			item.kind === 'separator'
				? { kind: 'separator' }
				: {
						kind: 'action',
						id: item.id,
						name: item.action.name,
						source: item.action.source,
						...(item.when ? { when: item.when } : {}),
					}
		);
		out[id] = { id: menu.id, items, hidden: [...menu.hidden] };
	}
	return out;
}

interface ActionsSetRequestPayload {
	request_id: string;
	scope: ActionsScope;
	action: UserAction;
}

interface ActionsImportRequestPayload {
	request_id: string;
	scope: ActionsScope;
	actions: UserAction[];
}

interface KeysSetRequestPayload {
	request_id: string;
	scope: ActionsScope;
	key: string;
	command: string;
	when?: string | null;
	key_scope?: 'app' | 'os' | null;
	platform?: 'mac' | 'other' | null;
}

interface KeysResolveRequestPayload {
	request_id: string;
	key: string;
	platform?: 'mac' | 'other' | null;
}

/** `ActionsValidationError` carries the WP-50 validator's own message
 *  (`{code} at {path}: {message}`); anything else falls back to its own
 *  `.message` (or a generic string) so a thrown non-Error still reports. */
function requestErrorMessage(err: unknown): string {
	if (err instanceof ActionsValidationError) return err.message;
	if (err instanceof Error) return err.message;
	return String(err);
}

async function reportRequestResult(requestId: string, result: unknown): Promise<void> {
	try {
		await iykeActionsRequestDone(requestId, result);
	} catch (err) {
		console.warn('[iyke] actions_request_done failed:', err);
	}
}

/** `POST /iyke/actions/set` round trip: `saveUserAction`, the same call the
 *  D-06 Editor tab's Save button makes. */
async function handleActionsSetRequest(payload: ActionsSetRequestPayload): Promise<void> {
	try {
		// The nested `action.scope` must agree with the outer file scope
		// (`E_SCOPE_MISMATCH`, G-ACTIONS §1.2) — set it from the wrapper so a
		// caller that only set one of the two still gets a consistent write.
		await saveUserAction(payload.scope, { ...payload.action, scope: payload.scope });
		await reportRequestResult(payload.request_id, { ok: true });
	} catch (err) {
		await reportRequestResult(payload.request_id, { ok: false, error: requestErrorMessage(err) });
	}
}

/** `POST /iyke/actions/import` round trip: `saveUserAction` per item, so one
 *  invalid action in a batch doesn't sink the rest. */
async function handleActionsImportRequest(payload: ActionsImportRequestPayload): Promise<void> {
	let added = 0;
	const errors: Array<{ id: string | null; error: string }> = [];
	for (const action of payload.actions) {
		try {
			await saveUserAction(payload.scope, { ...action, scope: payload.scope });
			added += 1;
		} catch (err) {
			errors.push({ id: action?.id ?? null, error: requestErrorMessage(err) });
		}
	}
	await reportRequestResult(payload.request_id, {
		ok: true,
		added,
		skipped: errors.length,
		errors,
	});
}

/** `POST /iyke/keys/set` round trip: `addKeybinding` — adds one positive
 *  rule (never a full rebind with a paired negative rule; see WP-62's
 *  report for this as a recorded, deliberate scope limitation). */
async function handleKeysSetRequest(payload: KeysSetRequestPayload): Promise<void> {
	try {
		await addKeybinding(payload.scope, {
			key: payload.key,
			command: payload.command,
			...(payload.when ? { when: payload.when } : {}),
			...(payload.key_scope ? { scope: payload.key_scope } : {}),
			...(payload.platform ? { platform: payload.platform } : {}),
		});
		await reportRequestResult(payload.request_id, { ok: true });
	} catch (err) {
		await reportRequestResult(payload.request_id, { ok: false, error: requestErrorMessage(err) });
	}
}

/** `GET /iyke/keys/resolve` round trip: the live `resolveKeypress()` winner
 *  for a key sequence — the hand-off's "what fires here" query. Never
 *  throws (an unparsable key just resolves to no candidates), so this
 *  reports straight through with no `ok`/`error` envelope. */
async function handleKeysResolveRequest(payload: KeysResolveRequestPayload): Promise<void> {
	const resolution = resolveKeypress(
		{ key: payload.key },
		undefined,
		payload.platform ?? undefined
	);
	await reportRequestResult(payload.request_id, resolution);
}

function pushShellState(
	activeMode: string,
	sidebarCollapsed: boolean,
	root: PaneNode,
	focusedId: PaneId,
	terminalTabs: TerminalTab[]
): void {
	const focused = findLeaf(root, focusedId);
	const view = focused?.tabs[focused.activeTabIdx];
	const route = view?.kind === 'route' ? view.path : null;
	const panes = buildPanesPayload(root, focusedId, terminalTabs);
	setShell({ mode: activeMode, route, panes, sidebarCollapsed }).catch((err) => {
		console.warn('[iyke] set_shell failed:', err);
	});
}

interface LeafSummary {
	id: string;
	focused: boolean;
	activeTabIdx: number;
	tabs: Array<{
		kind: string;
		title: string;
		pinned?: boolean;
		terminalId?: string;
		ptyId?: string;
	}>;
	/** Pkg id when the active tab is a /pkg/<id>/ route. */
	pkg?: string;
	/** Latest state the pkg iframe published (e.g. open-task selection). */
	state?: Record<string, unknown>;
}

interface PanesPayload {
	leaves: LeafSummary[];
	tree: PaneNode;
}

function buildPanesPayload(
	root: PaneNode,
	focusedId: PaneId,
	terminalTabs: TerminalTab[]
): PanesPayload {
	const ids = getLeafIdsInOrder(root);
	const leaves: LeafSummary[] = ids.map((id) => {
		const leaf = findLeaf(root, id);
		if (!leaf) {
			// Defensive — getLeafIdsInOrder pulled this id from the same
			// tree, so findLeaf should always succeed.
			return { id, focused: false, activeTabIdx: 0, tabs: [] };
		}
		const summary: LeafSummary = {
			id,
			focused: id === focusedId,
			activeTabIdx: leaf.activeTabIdx,
			tabs: leaf.tabs.map((t) => {
				const terminal =
					t.kind === 'terminal' ? terminalTabs.find((tab) => tab.id === t.sessionId) : null;
				return {
					kind: t.kind,
					title: viewTitle(t, terminal ?? null),
					...(t.pinned ? { pinned: true } : {}),
					...(t.kind === 'terminal' ? { terminalId: t.sessionId } : {}),
					...(terminal?.ptyId ? { ptyId: terminal.ptyId } : {}),
				};
			}),
		};
		// Surface the pkg id + its latest published state (selection etc.) for
		// pkg-route panes, so external callers can answer "what's open in this
		// pane" from `iyke state` alone. Pkg iframes register by pkg id —
		// see pkg-iframe-host.tsx Step 1c.
		const active = leaf.tabs[leaf.activeTabIdx];
		if (active?.kind === 'route') {
			const m = /^\/pkg\/([^/]+)/.exec(active.path);
			if (m) {
				summary.pkg = m[1];
				const reg = getIframe(m[1]);
				if (reg && Object.keys(reg.state).length > 0) summary.state = reg.state;
			}
		}
		return summary;
	});
	return { leaves, tree: root };
}

function viewTitle(view: PaneView, terminal: TerminalTab | null): string {
	switch (view.kind) {
		case 'route':
			return view.path;
		case 'terminal':
			// A bare session uuid told an agent nothing about which terminal it
			// was looking at. Name it the same way the tab strip does — the
			// foreground command isn't available on this path (no poll here), so
			// this is the spawn-time view; `GET /iyke/terminal/list` remains the
			// live, authoritative surface.
			return terminal
				? formatTerminalTitle({
						cwd: terminal.spec.cwd,
						argv: terminal.spec.cmd,
						title: terminal.title,
						exited: terminal.status === 'exited',
						home: getHomeSync(),
					}).label
				: view.sessionId;
		case 'artifact':
			return view.path;
		case 'artifact-studio':
			return 'artifact-studio';
		case 'scratchpad':
			return `${view.scope}/${view.name}`;
	}
}
