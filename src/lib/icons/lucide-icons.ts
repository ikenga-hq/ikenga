// Name-resolved lucide icons (a pin's `iconLucide`, an action's `icon`, a pkg
// manifest's `ui.views[0].icon`) all go through here.
//
// Two halves, deliberately separate:
//   - the NAME LIST is static and synchronous (`virtual:lucide-icon-names`,
//     built from the installed lucide-react by scripts/vite-plugin-lucide-icons.ts)
//     so "is this a real icon?" never waits on a network request;
//   - the ICON COMPONENTS come from ONE lazily-loaded chunk (the plugin's
//     `virtual:lucide-icon-map`), imported once and memoised. Icons the shell
//     imports by name (`import { Zap } from 'lucide-react'`) are unaffected.
//
// Until a name resolves, callers render their own fallback glyph, which keeps
// the layout stable.

import iconNames from 'virtual:lucide-icon-names';
import type { LucideIcon } from 'lucide-react';
import { useEffect, useReducer } from 'react';

/** Every kebab-case lucide icon name (aliases included). */
export const LUCIDE_ICON_NAMES: readonly string[] = iconNames;

const KNOWN_NAMES: ReadonlySet<string> = new Set(iconNames);

export function isLucideIconName(name: string): boolean {
	return KNOWN_NAMES.has(name);
}

type IconMapModule = typeof import('virtual:lucide-icon-map');

let mapModule: Promise<IconMapModule> | null = null;
const resolved = new Map<string, LucideIcon | null>();
const inflight = new Map<string, Promise<LucideIcon | null>>();

/** The single `import()` of the icon chunk. Memoised; a failed fetch is not
 *  cached so a later call can retry. */
function loadIconMap(): Promise<IconMapModule> {
	if (!mapModule) {
		mapModule = import('virtual:lucide-icon-map').catch((err) => {
			mapModule = null;
			throw err;
		});
	}
	return mapModule;
}

/** The icon if it has already resolved, else undefined. Never loads anything. */
export function peekLucideIcon(name: string): LucideIcon | undefined {
	return resolved.get(name) ?? undefined;
}

/** Resolve a kebab-case icon name to its component, loading the shared icon
 *  chunk on first use. Resolves null for an unknown name or a failed load. */
export function loadLucideIcon(name: string): Promise<LucideIcon | null> {
	if (!isLucideIconName(name)) return Promise.resolve(null);
	const hit = resolved.get(name);
	if (hit) return Promise.resolve(hit);
	let pending = inflight.get(name);
	if (!pending) {
		pending = loadIconMap()
			.then((m) => m.resolveIcon(name))
			.then((icon) => {
				resolved.set(name, icon ?? null);
				return icon ?? null;
			})
			.catch((err) => {
				console.error(`[lucide-icons] could not load icon "${name}"`, err);
				return null;
			})
			.finally(() => inflight.delete(name));
		inflight.set(name, pending);
	}
	return pending;
}

/** The component for `name`, or null while it loads / when it is unknown. */
export function useLucideIcon(name: string | null | undefined): LucideIcon | null {
	const [, rerender] = useReducer((n: number) => n + 1, 0);
	const known = name && isLucideIconName(name) ? name : null;
	const icon = known ? (peekLucideIcon(known) ?? null) : null;
	useEffect(() => {
		if (!known || icon) return;
		let live = true;
		void loadLucideIcon(known).then(() => live && rerender());
		return () => {
			live = false;
		};
	}, [known, icon]);
	return icon;
}

/** Test seam: forget everything memoised. */
export function resetLucideIconsForTests(): void {
	mapModule = null;
	resolved.clear();
	inflight.clear();
}
