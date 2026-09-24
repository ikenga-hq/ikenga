// Ikenga theme/mode/density/workspace store.
//
// Drives the data-* attribute set on <html> that @ikenga/tokens reads.
// Reconstructed 2026-05-06 — earlier history of this file was not present
// in the working tree at the Tasks-pkg cutover; this version restores the
// surface every existing consumer expects:
//
//   - useIkengaStore  (zustand store)
//   - installIkengaDomSync()  (one-time DOM-sync subscription)
//   - types: IkengaTheme, IkengaMode, IkengaDensity, IkengaTintStrength,
//            IkengaWorkspace
//
// Theme/mode/density/tintStrength also mirror into the file-backed settings
// contract so the user's appearance choices survive "Clear local data" (see
// `hydrateAppearanceFromRust`).

import { create } from 'zustand';
import { persist, createJSONStorage } from 'zustand/middleware';

import { settingsReadFile, settingsWriteField } from '@/lib/tauri-cmd';
import type { SettingsWriteOptions } from '@/lib/settings/types';
import { scopedPersistName } from '@/lib/window/window-context';

/** Palette variants. A=default (Iroko/dusk), B=Kola amber, C=verdigris. */
export type IkengaTheme = 'A' | 'B' | 'C';

/** Light/dark switch. The shell-default is dark.
 *
 * 'system' tracks the OS preference via `matchMedia` (installed by
 * `installIkengaDomSync`). The persisted store value stays 'system';
 * only the resolved `<html data-mode>` attribute flips between
 * light/dark in response to the media query. */
export type IkengaMode = 'light' | 'dark' | 'system';

/** The mode actually written to the DOM (i.e. 'system' resolved to
 * either 'light' or 'dark'). */
export type ResolvedIkengaMode = 'light' | 'dark';

/** Row height + body font density. */
export type IkengaDensity = 'compact' | 'comfortable' | 'spacious';

/** How aggressive the workspace tint reads on chrome. */
export type IkengaTintStrength = 'off' | 'subtle' | 'strong';

/** The workspace written to `<html data-workspace>` — one per rail noun
 *  (WP-03, g-state.md §1): Project · Chi · Ngwa · Settings. This store is
 *  the only writer of that attribute (`installIkengaDomSync` below). */
export type IkengaWorkspace = 'project' | 'chi' | 'ngwa' | 'settings';

/** Dormant tint names. The pre-v16 rail wrote these to `data-workspace`;
 *  nothing does any more, but their `--tint-<name>-*` variables stay in the
 *  design tokens for any pkg (or per-tab chrome, `tab-workspace.ts`) that
 *  opts into one. Never a valid `setWorkspace` argument. */
export type IkengaLegacyTint =
	| 'app'
	| 'files'
	| 'sessions'
	| 'artifact-grid'
	| 'pkgs'
	| 'mail'
	| 'outbox'
	| 'studio'
	| 'agents';

/** Any tint name the chrome may colour with: a rail workspace or a dormant
 *  legacy tint. */
export type IkengaTint = IkengaWorkspace | IkengaLegacyTint;

export const IKENGA_WORKSPACES: readonly IkengaWorkspace[] = Object.freeze([
	'project',
	'chi',
	'ngwa',
	'settings',
]);

function isIkengaWorkspace(w: unknown): w is IkengaWorkspace {
	return typeof w === 'string' && (IKENGA_WORKSPACES as readonly string[]).includes(w);
}

interface IkengaState {
	theme: IkengaTheme;
	mode: IkengaMode;
	density: IkengaDensity;
	tintStrength: IkengaTintStrength;
	workspace: IkengaWorkspace;
	setTheme: (t: IkengaTheme) => void;
	setMode: (m: IkengaMode) => void;
	setDensity: (d: IkengaDensity) => void;
	setTintStrength: (s: IkengaTintStrength) => void;
	setWorkspace: (w: IkengaWorkspace) => void;
	/** Pull durable appearance prefs from the personal/project settings
	 * files and overwrite local state. When no settings file exists yet,
	 * seed it once from the current localStorage-hydrated snapshot so
	 * existing users carry over. Called once at app boot from `main.tsx`. */
	hydrateAppearanceFromRust: () => Promise<void>;
}

const KV_THEME = 'appearance.theme' as const;
const KV_MODE = 'appearance.mode' as const;
const KV_DENSITY = 'appearance.density' as const;
const KV_TINT = 'appearance.tintStrength' as const;

type AppearanceField =
	| typeof KV_THEME
	| typeof KV_MODE
	| typeof KV_DENSITY
	| typeof KV_TINT;
type AppearanceValueMap = {
	[K in AppearanceField]: K extends typeof KV_THEME
		? IkengaTheme
		: K extends typeof KV_MODE
			? IkengaMode
			: K extends typeof KV_DENSITY
				? IkengaDensity
				: IkengaTintStrength;
};

let suppressKv = false;
let appearanceWriteQueue: Promise<void> = Promise.resolve();

function queueAppearanceWrite(options: SettingsWriteOptions): void {
	const next = appearanceWriteQueue
		.catch(() => {})
		.then(() => settingsWriteField(options));
	appearanceWriteQueue = next.then(
		() => undefined,
		() => undefined
	);
	void appearanceWriteQueue.catch(() => {});
}

function kvSet<K extends AppearanceField>(key: K, value: AppearanceValueMap[K]): void {
	if (suppressKv) return;
	queueAppearanceWrite({ scope: 'personal', field: key, value });
}

export const useIkengaStore = create<IkengaState>()(
	persist(
		(set, get) => ({
			theme: 'A',
			mode: 'dark',
			density: 'comfortable',
			tintStrength: 'subtle',
			workspace: 'project',
			setTheme: (theme) => {
				set({ theme });
				kvSet(KV_THEME, theme);
			},
			setMode: (mode) => {
				set({ mode });
				kvSet(KV_MODE, mode);
			},
			setDensity: (density) => {
				set({ density });
				kvSet(KV_DENSITY, density);
			},
			setTintStrength: (tintStrength) => {
				set({ tintStrength });
				kvSet(KV_TINT, tintStrength);
			},
			setWorkspace: (workspace) => set({ workspace }),
		hydrateAppearanceFromRust: async () => {
			let result: Awaited<ReturnType<typeof settingsReadFile>>;
			try {
				result = await settingsReadFile('personal');
			} catch {
				return;
			}
			if (!result.personalPresent && !result.projectPresent) {
				const s = get();
				suppressKv = true;
				try {
					queueAppearanceWrite({ scope: 'personal', field: KV_THEME, value: s.theme });
					queueAppearanceWrite({ scope: 'personal', field: KV_MODE, value: s.mode });
					queueAppearanceWrite({ scope: 'personal', field: KV_DENSITY, value: s.density });
					queueAppearanceWrite({
						scope: 'personal',
						field: KV_TINT,
						value: s.tintStrength,
					});
				} finally {
					suppressKv = false;
				}
				return;
			}
			const appearance = result.effective.appearance ?? {};
			const next: Partial<IkengaState> = {};
			const t = appearance.theme;
			if (t === 'A' || t === 'B' || t === 'C') next.theme = t;
			const m = appearance.mode;
			if (m === 'light' || m === 'dark' || m === 'system') next.mode = m;
			const d = appearance.density;
			if (d === 'compact' || d === 'comfortable' || d === 'spacious') {
				next.density = d;
			}
			const ts = appearance.tintStrength;
			if (ts === 'off' || ts === 'subtle' || ts === 'strong') {
				next.tintStrength = ts;
			}
			suppressKv = true;
			try {
				set(next);
			} finally {
				suppressKv = false;
			}
		},
		}),
		{
			// Window-namespaced (plans/multi-window WP-05) so a detached window's
			// instant-paint theme cache doesn't clobber the primary's via shared
			// localStorage. The authoritative appearance still comes from the
			// shared Rust settings_kv via `hydrateAppearanceFromRust`, so a
			// detached window converges on the primary's theme once that resolves.
			name: scopedPersistName('ikenga.theme'),
			storage: createJSONStorage(() => localStorage),
			// v2 (WP-03): `workspace` narrows to the four rail nouns. A v1 blob's
			// pre-v16 name (`app`, `files`, `pkgs`, …) must not reach
			// `data-workspace` for the frame before the rail's first sync.
			version: 2,
			migrate: (persisted) => {
				const p = (persisted ?? {}) as Partial<IkengaState>;
				if (!isIkengaWorkspace(p.workspace)) {
					p.workspace = (p.workspace as unknown) === 'pkgs' ? 'ngwa' : 'project';
				}
				return p as IkengaState;
			},
		}
	)
);

let installed = false;

/** Resolve a store mode (which may be 'system') to the literal mode
 *  written into `<html data-mode>`. Exported for unit tests. */
export function resolveIkengaMode(mode: IkengaMode, prefersDark: boolean): ResolvedIkengaMode {
	if (mode === 'system') return prefersDark ? 'dark' : 'light';
	return mode;
}

/** Subscribe the store to <html> data-attribute writes. Call once at app
 *  bootstrap (from `main.tsx`). Idempotent — second calls are no-ops.
 *
 *  When `mode === 'system'` the resolved attribute follows the OS
 *  `prefers-color-scheme` query and re-applies on `change`. The store
 *  itself still persists 'system' so the preference survives reload. */
export function installIkengaDomSync() {
	if (installed) return;
	installed = true;
	if (typeof document === 'undefined') return;

	// Prefer-dark media query. Safari < 14 lacks `addEventListener` on
	// MediaQueryList — fall back to the deprecated addListener.
	const mql =
		typeof window !== 'undefined' && typeof window.matchMedia === 'function'
			? window.matchMedia('(prefers-color-scheme: dark)')
			: null;

	const apply = (s: IkengaState) => {
		const html = document.documentElement;
		const prefersDark = !!mql?.matches;
		const resolved = resolveIkengaMode(s.mode, prefersDark);
		html.setAttribute('data-theme', s.theme);
		html.setAttribute('data-mode', resolved);
		html.setAttribute('data-mode-source', s.mode); // 'light' | 'dark' | 'system'
		html.setAttribute('data-density', s.density);
		html.setAttribute('data-tint-strength', s.tintStrength);
		html.setAttribute('data-workspace', s.workspace);
	};

	apply(useIkengaStore.getState());
	useIkengaStore.subscribe(apply);

	if (mql) {
		const onChange = () => apply(useIkengaStore.getState());
		if (typeof mql.addEventListener === 'function') {
			mql.addEventListener('change', onChange);
		} else if (
			typeof (
				mql as MediaQueryList & {
					addListener?: (l: () => void) => void;
				}
			).addListener === 'function'
		) {
			(mql as MediaQueryList & { addListener: (l: () => void) => void }).addListener(onChange);
		}
	}
}
