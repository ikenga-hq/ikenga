// honest-failure-states WP-2 — client state around WSL health: which panes
// dismissed the banner this episode, the running/finished fix per distro, and
// the pending confirm (D-5 / D-6). Keyed by the normalised distro
// (`normalizeWslDistro`). In memory only: an episode doesn't outlive the app.

import { create } from 'zustand';
import type { WslFixAction } from '@/lib/tauri-cmd';
import type { WslSessionSnapshot } from './tabs';

export type WslFixPhase = 'running' | 'relaunching' | 'done' | 'cancelled' | 'failed';

export interface WslFixRun {
	action: WslFixAction;
	phase: WslFixPhase;
	/** What to tell the user once it's over (`null` while running). */
	message: string | null;
	at: number;
}

export interface WslFixConfirm {
	action: WslFixAction;
	distro: string;
	/** The WSL sessions the shutdown will close (shown in the confirm). */
	sessions: WslSessionSnapshot[];
}

interface WslHealthUiState {
	/** distro → tab ids that dismissed the banner this episode. */
	dismissed: Record<string, string[]>;
	/** distro → the last fix run. */
	runs: Record<string, WslFixRun>;
	/** distro → a restart_networking ran this episode and didn't fix it. */
	restartTried: Record<string, boolean>;
	/** distro → unix ms of the last errno-forced probe. */
	errnoProbedAt: Record<string, number>;
	confirm: WslFixConfirm | null;

	dismiss: (distro: string, tabId: string) => void;
	/** Health is back to ok: the episode is over. */
	endEpisode: (distro: string) => void;
	setRun: (distro: string, run: WslFixRun | null) => void;
	markRestartTried: (distro: string) => void;
	markErrnoProbe: (distro: string, at: number) => void;
	setConfirm: (confirm: WslFixConfirm | null) => void;
}

function omit<T>(rec: Record<string, T>, key: string): Record<string, T> {
	if (!(key in rec)) return rec;
	const next = { ...rec };
	delete next[key];
	return next;
}

export const useWslHealthUi = create<WslHealthUiState>((set) => ({
	dismissed: {},
	runs: {},
	restartTried: {},
	errnoProbedAt: {},
	confirm: null,

	dismiss: (distro, tabId) =>
		set((s) => {
			const cur = s.dismissed[distro] ?? [];
			if (cur.includes(tabId)) return s;
			return { dismissed: { ...s.dismissed, [distro]: [...cur, tabId] } };
		}),
	endEpisode: (distro) =>
		set((s) => ({
			dismissed: omit(s.dismissed, distro),
			restartTried: omit(s.restartTried, distro),
		})),
	setRun: (distro, run) =>
		set((s) => ({ runs: run ? { ...s.runs, [distro]: run } : omit(s.runs, distro) })),
	markRestartTried: (distro) =>
		set((s) => ({ restartTried: { ...s.restartTried, [distro]: true } })),
	markErrnoProbe: (distro, at) =>
		set((s) => ({ errnoProbedAt: { ...s.errnoProbedAt, [distro]: at } })),
	setConfirm: (confirm) => set({ confirm }),
}));
