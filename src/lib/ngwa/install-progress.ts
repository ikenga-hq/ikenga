// Per-row install progress for the Store.
//
// One run per Store row (keyed by the row id, which is also the install id
// sent to Rust). A run moves through stages from two sources:
//   - the front end itself: queued (Update all), resolving (reading the
//     registry detail and dependency plan), and which plan step is running;
//   - the Rust installer's `pkg-install://progress` events: downloading
//     (bytes / percent), verifying, extracting, installing dependencies,
//     registering, starting services, done.
// Events arrive through the transport's `listen`, so they reach the desktop
// app today and a browser tab once the daemon forwards events; without them
// the row still shows the front-end stages with an indeterminate bar.

import { create } from 'zustand';
import { listen } from '@/lib/transport';
import type { ClassifiedInstallError } from './install-errors';

export const INSTALL_PROGRESS_EVENT = 'pkg-install://progress';

export type InstallStageId =
	| 'queued'
	| 'resolving'
	| 'downloading'
	| 'verifying'
	| 'extracting'
	| 'installing_deps'
	| 'registering'
	| 'starting'
	| 'done';

/** The Rust `InstallProgressEvent` (`pkg/install_progress.rs`). */
export interface InstallProgressEvent {
	installId: string;
	pkgId: string;
	stage: Exclude<InstallStageId, 'queued'>;
	label: string;
	percent?: number;
	bytes?: number;
	total?: number;
	detail?: string;
	cancellable: boolean;
}

export type InstallRunStatus = 'running' | 'failed' | 'cancelled' | 'done';

export interface InstallRun {
	key: string;
	/** Display name of what the row installs. */
	name: string;
	verb: 'install' | 'update';
	status: InstallRunStatus;
	stage: InstallStageId;
	label: string;
	/** 0–100, or null for an indeterminate bar. */
	percent: number | null;
	detail: string | null;
	/** The plan step in flight when a pkg brings dependencies. */
	step: { index: number; total: number; pkgId: string } | null;
	cancellable: boolean;
	/** Set when the user asked to cancel and the installer hasn't stopped yet. */
	cancelRequested: boolean;
	error: ClassifiedInstallError | null;
}

export type InstallProgressAction =
	| { type: 'start'; key: string; name: string; verb: InstallRun['verb']; queued?: boolean }
	| { type: 'resolving'; key: string }
	| { type: 'step'; key: string; index: number; total: number; pkgId: string }
	| { type: 'event'; event: InstallProgressEvent }
	| { type: 'cancel-requested'; key: string }
	| { type: 'succeeded'; key: string }
	| { type: 'failed'; key: string; error: ClassifiedInstallError }
	| { type: 'clear'; key: string };

export type InstallProgressState = Record<string, InstallRun>;

const STAGE_LABEL: Record<InstallStageId, string> = {
	queued: 'Queued',
	resolving: 'Resolving',
	downloading: 'Downloading',
	verifying: 'Verifying integrity',
	extracting: 'Extracting',
	installing_deps: 'Installing dependencies',
	registering: 'Registering',
	starting: 'Starting services',
	done: 'Installed',
};

/** Stages after which cancelling is no longer safe. */
const COMMITTED: ReadonlySet<InstallStageId> = new Set(['registering', 'starting', 'done']);

export function stageLabel(stage: InstallStageId): string {
	return STAGE_LABEL[stage];
}

function patch(state: InstallProgressState, key: string, p: Partial<InstallRun>): InstallProgressState {
	const run = state[key];
	if (!run) return state;
	return { ...state, [key]: { ...run, ...p } };
}

export function installProgressReducer(
	state: InstallProgressState,
	action: InstallProgressAction
): InstallProgressState {
	switch (action.type) {
		case 'start': {
			const stage: InstallStageId = action.queued ? 'queued' : 'resolving';
			return {
				...state,
				[action.key]: {
					key: action.key,
					name: action.name,
					verb: action.verb,
					status: 'running',
					stage,
					label: STAGE_LABEL[stage],
					percent: null,
					detail: null,
					step: null,
					cancellable: true,
					cancelRequested: false,
					error: null,
				},
			};
		}
		case 'resolving':
			return patch(state, action.key, {
				stage: 'resolving',
				label: STAGE_LABEL.resolving,
				percent: null,
				detail: null,
			});
		case 'step':
			return patch(state, action.key, {
				step: { index: action.index, total: action.total, pkgId: action.pkgId },
			});
		case 'event': {
			const ev = action.event;
			const run = state[ev.installId];
			// Late events for a run that already settled (or was cleared) are
			// dropped; so are events for rows this window isn't tracking.
			if (!run || run.status !== 'running') return state;
			const percent = typeof ev.percent === 'number' ? Math.max(0, Math.min(100, ev.percent)) : null;
			return patch(state, ev.installId, {
				stage: ev.stage,
				label: ev.label || STAGE_LABEL[ev.stage],
				percent,
				// A detail line belongs to its stage: keep it only while the
				// stage is unchanged and the event didn't send a new one.
				detail: ev.detail ?? (run.stage === ev.stage ? run.detail : null),
				cancellable: ev.cancellable && !COMMITTED.has(ev.stage),
			});
		}
		case 'cancel-requested': {
			const run = state[action.key];
			if (!run || run.status !== 'running' || !run.cancellable) return state;
			return patch(state, action.key, { cancelRequested: true });
		}
		case 'succeeded':
			return patch(state, action.key, {
				status: 'done',
				stage: 'done',
				label: STAGE_LABEL.done,
				percent: 100,
				cancellable: false,
				error: null,
			});
		case 'failed':
			return patch(state, action.key, {
				status: action.error.kind === 'cancelled' ? 'cancelled' : 'failed',
				cancellable: false,
				cancelRequested: false,
				error: action.error,
			});
		case 'clear': {
			if (!state[action.key]) return state;
			const { [action.key]: _drop, ...rest } = state;
			return rest;
		}
	}
}

interface InstallProgressStore {
	runs: InstallProgressState;
	dispatch: (action: InstallProgressAction) => void;
}

export const useInstallProgressStore = create<InstallProgressStore>((set) => ({
	runs: {},
	dispatch: (action) => set((s) => ({ runs: installProgressReducer(s.runs, action) })),
}));

export function dispatchInstallProgress(action: InstallProgressAction): void {
	useInstallProgressStore.getState().dispatch(action);
}

export function useInstallRun(key: string | null | undefined): InstallRun | null {
	return useInstallProgressStore((s) => (key ? (s.runs[key] ?? null) : null));
}

let subscription: Promise<() => void> | null = null;

/** Start forwarding the installer's progress events into the store. Safe to
 *  call from every mount; the first call subscribes once for the page. */
export function ensureInstallProgressSubscription(): void {
	if (subscription) return;
	subscription = listen<InstallProgressEvent>(INSTALL_PROGRESS_EVENT, (ev) => {
		if (ev.payload?.installId) dispatchInstallProgress({ type: 'event', event: ev.payload });
	}).catch((err) => {
		console.warn('[install-progress] subscription failed:', err);
		subscription = null;
		return () => {};
	});
}

/** Format bytes received for the progress line: "1.2 / 3.4 MB". */
export function formatTransfer(bytes?: number | null, total?: number | null): string | null {
	if (typeof bytes !== 'number') return null;
	const mb = (n: number) => (n / (1024 * 1024)).toFixed(1);
	return typeof total === 'number' && total > 0 ? `${mb(bytes)} / ${mb(total)} MB` : `${mb(bytes)} MB`;
}
