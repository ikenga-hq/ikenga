// WP-50 typed client for the actions / keybindings file layer and the
// project-trust record. Every write goes through the Rust validator
// (`src-tauri/src/actions/schema.rs`) — the one WP-58..62 share.
//
// This is the file layer, not the effective model: it hands both user
// layers over in load order (personal, then project — G-ACTIONS §2.1). The
// merge (default < package < personal < project, negative rules, held
// project rules) is WP-52's `merge.ts` / `store.ts`.

import { invoke, listen, type UnlistenFn } from '@/lib/tauri-cmd';
import {
	ACTIONS_CHANGED_EVENT,
	type ActionsChangeEvent,
	type ActionsDocument,
	type ActionsFileKind,
	type ActionsFilesResult,
	type ActionsScope,
	type ActionsTrustGrant,
	type ActionsTrustRevoke,
	type ActionsTrustStatus,
	type ActionsWriteResult,
	type KeybindingsDocument,
	type KeybindingsTrust,
	type UserAction,
	type Validation,
} from './types';

export * from './types';

/** Thrown when the validator refuses a write; the file was left untouched. */
export class ActionsValidationError extends Error {
	readonly validation: Validation;
	readonly result: ActionsWriteResult;

	constructor(result: ActionsWriteResult) {
		const first = result.validation.errors[0];
		super(
			first
				? `${result.path}: ${first.code} at ${first.path || '/'}: ${first.message}`
				: `${result.path}: refused`
		);
		this.name = 'ActionsValidationError';
		this.validation = result.validation;
		this.result = result;
	}
}

export async function readActionsFiles(projectId?: string | null): Promise<ActionsFilesResult> {
	return invoke<ActionsFilesResult>('actions_read_files', { projectId: projectId ?? null });
}

function checked(result: ActionsWriteResult): ActionsWriteResult {
	if (!result.written) throw new ActionsValidationError(result);
	return result;
}

/** Validates and atomically writes a whole `actions.json`. */
export async function writeActionsFile(
	scope: ActionsScope,
	document: ActionsDocument,
	projectId?: string | null
): Promise<ActionsWriteResult> {
	return checked(
		await invoke<ActionsWriteResult>('actions_write', {
			scope,
			document,
			projectId: projectId ?? null,
		})
	);
}

/** Validates and atomically writes a whole `keybindings.json`. */
export async function writeKeybindingsFile(
	scope: ActionsScope,
	document: KeybindingsDocument,
	projectId?: string | null
): Promise<ActionsWriteResult> {
	return checked(
		await invoke<ActionsWriteResult>('keybindings_write', {
			scope,
			document,
			projectId: projectId ?? null,
		})
	);
}

/** Creates the file if absent and opens it with the OS. Returns its path. */
export async function openActionsFile(
	file: ActionsFileKind,
	scope: ActionsScope,
	projectId?: string | null
): Promise<string> {
	return invoke<string>('actions_open_file', { file, scope, projectId: projectId ?? null });
}

export async function actionsTrustStatus(projectId?: string | null): Promise<ActionsTrustStatus> {
	return invoke<ActionsTrustStatus>('actions_trust_status', { projectId: projectId ?? null });
}

/** Pins the shown hashes. Refused whole if any file changed since it was shown. */
export async function actionsTrustGrant(
	request: ActionsTrustGrant,
	projectId?: string | null
): Promise<ActionsTrustStatus> {
	return invoke<ActionsTrustStatus>('actions_trust_grant', {
		request: { actions: request.actions ?? [], keybindings: request.keybindings ?? null },
		projectId: projectId ?? null,
	});
}

/** Removes pins. An empty request revokes everything the project has. */
export async function actionsTrustRevoke(
	request: ActionsTrustRevoke = {},
	projectId?: string | null
): Promise<ActionsTrustStatus> {
	return invoke<ActionsTrustStatus>('actions_trust_revoke', {
		request: {
			actionIds: request.actionIds ?? null,
			keybindings: request.keybindings ?? null,
		},
		projectId: projectId ?? null,
	});
}

/**
 * Subscribes to `actions://changed`: on-disk edits, 250 ms debounced by the
 * Rust watcher (which is also how a successful write is announced — once,
 * not by the write command), and trust grants / revokes (`reason: 'trust'`).
 * Bursts are coalesced into one callback.
 */
export function watchActionsFiles(
	onChange: (events: ActionsChangeEvent[]) => void | Promise<void>
): Promise<UnlistenFn> {
	let timer: ReturnType<typeof setTimeout> | null = null;
	let pending: ActionsChangeEvent[] = [];
	let active = true;
	let unlisten: UnlistenFn | null = null;
	return listen<ActionsChangeEvent>(ACTIONS_CHANGED_EVENT, (event) => {
		if (!active) return;
		pending.push(event.payload);
		if (timer) clearTimeout(timer);
		timer = setTimeout(() => {
			timer = null;
			if (!active) return;
			const batch = pending;
			pending = [];
			void Promise.resolve(onChange(batch)).catch(() => {});
		}, 50);
	}).then((stop) => {
		unlisten = stop;
		return () => {
			if (!active) return;
			active = false;
			if (timer) {
				clearTimeout(timer);
				timer = null;
			}
			pending = [];
			unlisten?.();
		};
	});
}

// --- pure helpers over a read result -------------------------------------------

/** DEC-65: whether the project's keybindings rules are held out of the keymap. */
export function projectKeybindingsHeld(trust: KeybindingsTrust | null | undefined): boolean {
	return trust?.state === 'untrusted' || trust?.state === 'changed';
}

export interface ResolvedUserAction {
	action: UserAction;
	scope: ActionsScope;
	/** A personal action whose id the project redefines (shown "overridden by project"). */
	overriddenBy?: 'project';
}

/**
 * User actions in force, project over personal (§1.2: a project action with
 * the same id wins whole, no field merge). Returns the winner per id plus
 * the overridden personal ones, personal first then project, in file order.
 */
export function resolveUserActions(files: ActionsFilesResult): ResolvedUserAction[] {
	const personal = files.personal.actions.document?.actions ?? [];
	const project = files.project?.actions.document?.actions ?? [];
	const projectIds = new Set(project.map((action) => action.id));
	return [
		...personal.map(
			(action): ResolvedUserAction =>
				projectIds.has(action.id)
					? { action, scope: 'personal', overriddenBy: 'project' }
					: { action, scope: 'personal' }
		),
		...project.map((action): ResolvedUserAction => ({ action, scope: 'project' })),
	];
}
