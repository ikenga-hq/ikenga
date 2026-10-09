/**
 * The local-echo preference: Auto / Always / Off.
 *
 * Per browser, not per workspace: whether prediction helps depends on this
 * device's network path to the server, which the server-side settings cannot
 * know. Stored in `localStorage`, and read defensively — storage can throw
 * outright (Safari private mode, blocked site data), and a terminal must not
 * fail to mount over a preference.
 */

import { create } from 'zustand';
import type { LocalEchoMode } from './engine';

export const LOCAL_ECHO_STORAGE_KEY = 'ikenga.terminal.localEcho';

const MODES: readonly LocalEchoMode[] = ['auto', 'always', 'off'];

export function parseLocalEchoMode(raw: unknown): LocalEchoMode {
	return typeof raw === 'string' && (MODES as readonly string[]).includes(raw)
		? (raw as LocalEchoMode)
		: 'auto';
}

function readStored(): LocalEchoMode {
	try {
		return parseLocalEchoMode(localStorage.getItem(LOCAL_ECHO_STORAGE_KEY));
	} catch {
		return 'auto';
	}
}

interface LocalEchoSettingsState {
	mode: LocalEchoMode;
	setMode: (mode: LocalEchoMode) => void;
}

export const useLocalEchoSettings = create<LocalEchoSettingsState>((set) => ({
	mode: readStored(),
	setMode: (mode) => {
		try {
			localStorage.setItem(LOCAL_ECHO_STORAGE_KEY, mode);
		} catch {
			// Memory-only for this page load.
		}
		set({ mode });
	},
}));
