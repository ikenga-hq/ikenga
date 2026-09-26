// WP-53 — the `open` run kind (G-ACTIONS §8.1). The interpolated `url`
// (values percent-encoded, §8.2) resolves to one of:
//
//   • `/…`                 a shell route → the focused pane (`goto`'s two
//                          calls: re-sync the mode, then `navigateFocused`)
//   • `pkg://<id>/<path>`  a package view → the pane route `/pkg/<id>/<path>`
//   • `http(s)://…`, `mailto:…`  → the OS (`shell:allow-open`)
//   • anything else        → refused with a reason (`file:`, `javascript:`, …)
//
// `goto` itself lives in `src/shell/menu/tree.tsx`; it is mirrored here
// rather than imported so the menus (WP-55), which will call this runner,
// never form an import cycle with it.

import { usePaneStore } from '@/lib/panes/pane-store';
import { modeForRoute } from '@/lib/shell/mode-routes';
import { useShellStore } from '@/lib/shell/shell-store';
import { openExternalUrl } from '@/lib/transport/shims';
import type { ActionRun } from '../types';

export type OpenRun = Extract<ActionRun, { kind: 'open' }>;

export type OpenTarget =
	| { kind: 'route'; path: string }
	| { kind: 'external'; url: string }
	| { kind: 'refused'; message: string };

const PKG_ID_RE = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;

export function classifyOpenUrl(url: string): OpenTarget {
	const value = url.trim();
	if (value === '') return { kind: 'refused', message: 'The action has no URL to open.' };
	if (value.startsWith('/') && !value.startsWith('//')) return { kind: 'route', path: value };
	if (value.startsWith('pkg://')) {
		const rest = value.slice('pkg://'.length);
		const slash = rest.indexOf('/');
		const pkgId = slash < 0 ? rest : rest.slice(0, slash);
		const tail = slash < 0 ? '' : rest.slice(slash + 1);
		if (!PKG_ID_RE.test(pkgId)) return { kind: 'refused', message: `“${value}” names no package.` };
		if (tail.split('/').includes('..')) return { kind: 'refused', message: `“${value}” is not a package view.` };
		return { kind: 'route', path: `/pkg/${pkgId}${tail ? `/${tail}` : ''}` };
	}
	let parsed: URL;
	try {
		parsed = new URL(value);
	} catch {
		return { kind: 'refused', message: `“${value}” is not a route, package view or web address.` };
	}
	if (parsed.protocol === 'http:' || parsed.protocol === 'https:' || parsed.protocol === 'mailto:') {
		return { kind: 'external', url: parsed.href };
	}
	return { kind: 'refused', message: `${parsed.protocol} links are not opened by actions.` };
}

/** `goto` (`menu/tree.tsx`): sync the mode for the route, then navigate. */
export function gotoRoute(path: string): void {
	const mode = modeForRoute(path);
	if (mode) useShellStore.getState().setActiveMode(mode);
	usePaneStore.getState().navigateFocused(path);
}

export async function openTarget(target: Exclude<OpenTarget, { kind: 'refused' }>): Promise<void> {
	if (target.kind === 'route') gotoRoute(target.path);
	else await openExternalUrl(target.url);
}
