// `viewLabel`'s `route` case used to return the raw path segment for every
// route, including `/pkg/<id>` and `/ngwa/item/<id>` — a reverse-DNS id, not
// a human label. `resolveDisplayName` (optional, same shape as the existing
// `resolveTerminal`) resolves those two shapes to a display name; everything
// else is unaffected. See `use-pane-display-names.ts` for the real resolver;
// these tests use a plain stub so they don't need react-query or Tauri.

import { describe, expect, it } from 'vitest';
import type { PaneView } from '@/lib/panes/types';
import { shouldCapitalizeLabel, viewLabel } from './pane-view-label';
import type { PaneDisplayNameResolver } from './use-pane-display-names';

const resolver: PaneDisplayNameResolver = (id) => {
	if (id === 'com.ikenga.studio') return 'Studio';
	if (id === 'com.ikenga.engine-claude-code') return 'Claude Code';
	return undefined;
};

function route(path: string): PaneView {
	return { kind: 'route', path };
}

describe('viewLabel — /pkg and /ngwa/item display-name resolution', () => {
	it('resolves /pkg/<id> to its display name', () => {
		expect(viewLabel(route('/pkg/com.ikenga.studio'), undefined, resolver)).toBe('Studio');
	});

	it('resolves /pkg/<id>/<sub-path> to the same display name', () => {
		expect(viewLabel(route('/pkg/com.ikenga.studio/settings'), undefined, resolver)).toBe('Studio');
	});

	it('resolves /ngwa/item/<id> to its display name', () => {
		expect(viewLabel(route('/ngwa/item/com.ikenga.engine-claude-code'), undefined, resolver)).toBe(
			'Claude Code'
		);
	});

	it('falls back to the raw id, unchanged, when the resolver has nothing for it', () => {
		expect(viewLabel(route('/pkg/com.ikenga.unknown-pkg'), undefined, resolver)).toBe(
			'com.ikenga.unknown-pkg'
		);
	});

	it('falls back to the raw id, unchanged, when no resolver is passed at all', () => {
		expect(viewLabel(route('/pkg/com.ikenga.studio'))).toBe('com.ikenga.studio');
	});

	it('decodes a URI-encoded id before resolving', () => {
		const r: PaneDisplayNameResolver = (id) =>
			id === 'com.ikenga.has space' ? 'Has Space' : undefined;
		expect(viewLabel(route('/pkg/com.ikenga.has%20space'), undefined, r)).toBe('Has Space');
	});

	it('leaves a non-pkg, non-ngwa-item route label exactly as before', () => {
		expect(viewLabel(route('/settings/secrets'), undefined, resolver)).toBe('secrets');
		expect(viewLabel(route('/mail/triage-queue'), undefined, resolver)).toBe('triage queue');
	});

	it('still returns Dashboard for the root route', () => {
		expect(viewLabel(route('/'), undefined, resolver)).toBe('Dashboard');
	});
});

describe('shouldCapitalizeLabel', () => {
	it('never capitalizes a dotted (unresolved id) label', () => {
		const view = route('/pkg/com.ikenga.unknown-pkg');
		const label = viewLabel(view, undefined, resolver);
		expect(shouldCapitalizeLabel(view, label, resolver)).toBe(false);
	});

	it('never capitalizes a resolved display name', () => {
		const view = route('/pkg/com.ikenga.studio');
		const label = viewLabel(view, undefined, resolver);
		expect(shouldCapitalizeLabel(view, label, resolver)).toBe(false);
	});

	it('never capitalizes any label containing a literal dot, resolved or not', () => {
		const dotted: PaneDisplayNameResolver = (id) =>
			id === 'com.ikenga.studio' ? 'v1.2 Studio' : undefined;
		const view = route('/pkg/com.ikenga.studio');
		const label = viewLabel(view, undefined, dotted);
		expect(label).toBe('v1.2 Studio');
		expect(shouldCapitalizeLabel(view, label, dotted)).toBe(false);
	});

	it('still capitalizes an ordinary route label', () => {
		const view = route('/settings/secrets');
		const label = viewLabel(view, undefined, resolver);
		expect(shouldCapitalizeLabel(view, label, resolver)).toBe(true);
	});

	it('never capitalizes a terminal label', () => {
		const view: PaneView = { kind: 'terminal', sessionId: 'sess-1' };
		expect(shouldCapitalizeLabel(view, 'claude · shell')).toBe(false);
	});
});
