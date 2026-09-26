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
	actionsTrustStatus,
	addKeybinding,
	type ActionsScope,
	type ActionTrust,
	type EffectiveAction,
	type EffectiveKeymap,
	type EffectiveModel,
	getEffectiveKeymap,
	type HeldKeybinding,
	getEffectiveModel,
	resolveKeypress,
	saveUserAction,
	subscribeEffectiveModel,
	subscribeKeymap,
	type UserAction,
	type Validation,
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
			iykeSetFrame({ keymap: keymapPayload(getEffectiveKeymap()) }).catch((err) => {
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
		pushActionsFrame(getEffectiveModel());
		return subscribeEffectiveModel(pushActionsFrame);
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
 *  plus the human label and the key hint resolved for this platform.
 *
 * WP-62 review (S3, DEC-65): also projects the project rules a live user
 * would see "held until trusted" in the Keys tab — `EffectiveKeymap.held` —
 * so `iyke keys list` doesn't silently drop what an untrusted project's
 * `keybindings.json` asked for. A held row carries no working key (it fires
 * nothing, DEC-65) and is marked `status: "held"` with the trust state that
 * holds it. */
export function keymapPayload(
	keymap: Pick<EffectiveKeymap, 'entries' | 'held'> = getEffectiveKeymap()
): IykeKeymapEntry[] {
	const active: IykeKeymapEntry[] = keymap.entries.map((e) => ({
		command: e.command,
		key: e.key,
		when: e.when,
		source: e.source,
		label: e.label,
		key_label: formatKeyLabel(e.key),
		...(e.platformOnly ? { platform_only: e.platformOnly } : {}),
	}));
	const held: IykeKeymapEntry[] = (keymap.held ?? []).map((h) => heldKeymapRow(h));
	return [...active, ...held];
}

function heldKeymapRow(h: HeldKeybinding): IykeKeymapEntry {
	const command = h.rule.command.startsWith('-') ? h.rule.command.slice(1) : h.rule.command;
	return {
		command,
		key: h.rule.key,
		when: h.rule.when ?? '',
		source: 'project',
		label: command,
		key_label: formatKeyLabel(h.rule.key),
		...(h.rule.platform ? { platform_only: h.rule.platform } : {}),
		status: 'held',
		trust: h.trust,
	};
}

// ─── WP-62: `iyke` actions / menus / keys mirror + write round trips ────────

/** S6: `menuIdsFor` (`menus.ts`) only materializes a `section/<id>` menu
 *  that some placement or override actually names — a section sitting at
 *  its plain defaults (no package/user action, no override) never lands in
 *  `model.menus.ids`, so it never reached the pushed mirror and
 *  `GET /iyke/menus/:id` 404'd it despite `getEffectiveMenu(id)` resolving
 *  it fine. Explorer's own section list (already synced to iyke separately,
 *  `GET /iyke/explorer/sections`) is the ground truth for which
 *  `section/<id>` ids currently exist, so every one of them is folded into
 *  the push too. */
function explorerSectionMenuIds(): string[] {
	return useShellStore.getState().explorerSections.map((s) => `section/${s.id}`);
}

/** `GET /iyke/actions` + `GET /iyke/menus/:id` push. Trust status
 * (`ActionsTrustStatus`) is its own WP-50 record, not part of the merged
 * `EffectiveModel` (S3, DEC-55) — fetched only when the model actually has
 * a project action to annotate, so the common (no project actions) case
 * stays a plain synchronous push. */
function pushActionsFrame(model: EffectiveModel): void {
	const menus = menusMirrorPayload(model, explorerSectionMenuIds());
	const hasProjectActions = model.projectId != null && model.actions.some((a) => a.source === 'project');
	if (!hasProjectActions) {
		iykeSetActionsFrame({
			actions: actionsMirrorPayload(model.actions),
			menus,
		}).catch((err) => {
			console.warn('[iyke] set_actions_frame failed:', err);
		});
		return;
	}
	void (async () => {
		try {
			const trust = await projectActionTrustMap(model.projectId);
			await iykeSetActionsFrame({
				actions: actionsMirrorPayload(model.actions, trust),
				menus,
			});
		} catch (err) {
			console.warn('[iyke] set_actions_frame failed:', err);
		}
	})();
}

/** `ActionTrust.state` by action id for `projectId` (null on any failure —
 *  the caller falls back to `actionsMirrorPayload`'s own untrusted default). */
async function projectActionTrustMap(
	projectId: string | null
): Promise<Map<string, ActionTrust['state']> | null> {
	if (!projectId) return null;
	try {
		const status = await actionsTrustStatus(projectId);
		return new Map(status.actions.map((a) => [a.id, a.state]));
	} catch (err) {
		console.warn('[iyke] actions_trust_status failed:', err);
		return null;
	}
}

/** The rows `GET /iyke/actions` serves — a flattened projection of
 *  `EffectiveAction[]` (G-ACTIONS-API). Opaque to Rust beyond this shape
 *  (`IykeActionMirror`, `src/lib/tauri-cmd.ts`).
 *
 * WP-62 review (S3, DEC-55): `projectTrust` carries a project action's
 * `ActionTrust.state`, keyed by id. Fail closed to `untrusted` for a
 * project action the trust record has no entry for yet — same rule as the
 * `ActionsTrustStatus` doc comment ("an action id missing from `actions` is
 * untrusted"). */
export function actionsMirrorPayload(
	actions: EffectiveAction[],
	projectTrust?: ReadonlyMap<string, ActionTrust['state']> | null
): IykeActionMirror[] {
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
		...(a.source === 'project' ? { trust_state: projectTrust?.get(a.id) ?? 'untrusted' } : {}),
	}));
}

/** The rows `GET /iyke/menus/:id` serves — every menu id the effective
 *  model currently knows, keyed the same way `EffectiveMenus.get()` is.
 *  `extraIds` (S6) covers `section/<id>` menus `model.menus.ids` doesn't
 *  enumerate on its own (`explorerSectionMenuIds`) — `.get()` still
 *  resolves them from their defaults, so pushing them here is enough to
 *  keep `GET /iyke/menus/:id` from 404ing a section that just hasn't been
 *  customized yet. */
export function menusMirrorPayload(
	model: EffectiveModel,
	extraIds: readonly string[] = []
): Record<string, IykeMenuMirror> {
	const out: Record<string, IykeMenuMirror> = {};
	const ids = new Set([...model.menus.ids, ...extraIds]);
	for (const id of ids) {
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

export interface ActionsImportRequestPayload {
	request_id: string;
	scope: ActionsScope;
	actions: UserAction[];
	/** An existing id is skipped unless `overwrite: true` (S4, D-06 import). */
	overwrite?: boolean;
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

/** The warnings a scope's file carried on its last read (§1.6) — surfaced on
 *  a successful write too (S5), not just refused ones. `saveUserAction` /
 *  `addKeybinding` return `void` (the frozen G-ACTIONS-API), so this reads
 *  the post-write validation back off `EffectiveModel.files` rather than
 *  the write call itself. */
function fileWarnings(scope: ActionsScope, kind: 'actions' | 'keybindings'): Validation['warnings'] {
	const files = getEffectiveModel().files;
	const scopeFiles = scope === 'personal' ? files?.personal : files?.project;
	return scopeFiles?.[kind].validation.warnings ?? [];
}

/** A project action's live trust state (fail closed to `untrusted`, same as
 *  `actionsMirrorPayload`) — looked up fresh so a write's response reflects
 *  the record as it stands right after the write, not a stale push. */
async function projectActionTrustState(actionId: string): Promise<string> {
	const model = getEffectiveModel();
	const trust = await projectActionTrustMap(model.projectId);
	return trust?.get(actionId) ?? 'untrusted';
}

/** `POST /iyke/actions/set` round trip: `saveUserAction`, the same call the
 *  D-06 Editor tab's Save button makes. */
async function handleActionsSetRequest(payload: ActionsSetRequestPayload): Promise<void> {
	try {
		// The nested `action.scope` must agree with the outer file scope
		// (`E_SCOPE_MISMATCH`, G-ACTIONS §1.2) — set it from the wrapper so a
		// caller that only set one of the two still gets a consistent write.
		await saveUserAction(payload.scope, { ...payload.action, scope: payload.scope });
		const result: Record<string, unknown> = {
			ok: true,
			warnings: fileWarnings(payload.scope, 'actions'),
		};
		// S3 (DEC-55): a project action write reports its trust state so a
		// caller knows at once whether the run it just saved will refuse.
		if (payload.scope === 'project') {
			result.trust = await projectActionTrustState(payload.action.id);
		}
		await reportRequestResult(payload.request_id, result);
	} catch (err) {
		await reportRequestResult(payload.request_id, {
			ok: false,
			error: requestErrorMessage(err),
			// S5: the full `{errors, warnings}` (§1.6), not just `.message`, so
			// `write_result_to_response` can send it as the 422 body.
			...(err instanceof ActionsValidationError ? { validation: err.validation } : {}),
		});
	}
}

/** The ids already present in `scope`'s own `actions.json` — in force or
 *  shadowed (a personal id the project redefines, §1.2, still occupies the
 *  personal file). An import's skip/overwrite check (S4) is against this
 *  set, not the whole effective id space (built-ins/packages can't be
 *  imported into here in the first place — the validator would refuse
 *  `E_ID_BUILTIN`). */
export function existingActionIds(scope: ActionsScope, model: EffectiveModel): Set<string> {
	const ids = model.actions.filter((a) => a.source === scope).map((a) => a.id);
	if (scope === 'personal') {
		ids.push(...model.shadowedActions.filter((a) => a.source === 'personal').map((a) => a.id));
	}
	return new Set(ids);
}

/** `POST /iyke/actions/import` round trip: add / skip / conflict like D-06's
 *  import (G-ACTIONS §1.6, the brief's "as D-06 import does"): an existing
 *  id is skipped unless `overwrite: true`. Each surviving item still goes
 *  through its own `saveUserAction` call — not the single `editActions`
 *  write pass the review round asked for — because that pass is the one
 *  place the frozen store serializes writes against concurrent edits
 *  (`store.ts`'s module-level write chain); reimplementing it against the
 *  lower-level file client here would drop that guarantee. See the WP-62
 *  report for this as recorded drift; `actions_routes.rs`'s import timeout
 *  scales with the item count to cover the resulting per-item round trips. */
export async function handleActionsImportRequest(payload: ActionsImportRequestPayload): Promise<void> {
	const overwrite = payload.overwrite === true;
	const model = getEffectiveModel();
	const existing = existingActionIds(payload.scope, model);

	const added: string[] = [];
	const skipped: string[] = [];
	const errors: Array<{ id: string | null; error: string }> = [];

	for (const action of payload.actions) {
		if (action?.id && existing.has(action.id) && !overwrite) {
			skipped.push(action.id);
			continue;
		}
		try {
			await saveUserAction(payload.scope, { ...action, scope: payload.scope });
			if (action?.id) {
				added.push(action.id);
				existing.add(action.id);
			}
		} catch (err) {
			errors.push({ id: action?.id ?? null, error: requestErrorMessage(err) });
		}
	}

	await reportRequestResult(payload.request_id, { ok: true, added, skipped, errors });
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
		await reportRequestResult(payload.request_id, {
			ok: true,
			warnings: fileWarnings(payload.scope, 'keybindings'),
			// S3 (DEC-65): a project keybinding write is always written held
			// until the project's keybindings are (re-)trusted.
			...(payload.scope === 'project' ? { held: true } : {}),
		});
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
