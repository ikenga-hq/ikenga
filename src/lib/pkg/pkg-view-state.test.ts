// WP-45 — pure half of the D-08 `pkg-view` states. Written, not run
// (DEC-50: build/test is WP-47's).

import { describe, expect, it } from 'vitest';
import {
	addedCapabilityLines,
	blockedInfoFromNavigation,
	blockedInfoFromViolation,
	handshakeSteps,
	isBlockingViolation,
	itemDetailPath,
	pkgIdFromRoutePath,
	VIOLATION_LOG_PATH,
} from './pkg-view-state';

describe('handshakeSteps', () => {
	const statuses = (phase: 'fetch' | 'handshake' | 'ready') =>
		handshakeSteps(phase, 'com.ikenga.studio', 'dist/index.html').map((s) => s.status);

	it('fetch: route resolved, html fetch in flight', () => {
		expect(statuses('fetch')).toEqual(['done', 'now', 'todo', 'todo']);
	});

	it('handshake: html fetched, bridge handshaking', () => {
		expect(statuses('handshake')).toEqual(['done', 'done', 'now', 'todo']);
	});

	it('ready: every step done', () => {
		expect(statuses('ready')).toEqual(['done', 'done', 'done', 'done']);
	});

	it('names the real pkg id and source', () => {
		const [resolve, fetch] = handshakeSteps('fetch', 'com.ikenga.studio', 'dist/grid.html');
		expect(resolve.label).toContain('com.ikenga.studio');
		expect(fetch.label).toContain('dist/grid.html');
	});

	it('has exactly one `now` step while loading', () => {
		for (const phase of ['fetch', 'handshake'] as const) {
			expect(handshakeSteps(phase, 'p', 's').filter((s) => s.status === 'now')).toHaveLength(1);
		}
	});
});

describe('isBlockingViolation', () => {
	it('navigation directives always block', () => {
		for (const d of ['frame-src', 'child-src', 'form-action', 'navigate-to']) {
			expect(isBlockingViolation({ blockedURI: 'https://x', effectiveDirective: d }, true)).toBe(
				true
			);
		}
	});

	it('a script block before the view initialised blocks (its boot was stopped)', () => {
		expect(
			isBlockingViolation({ blockedURI: 'https://cdn.x/a.js', effectiveDirective: 'script-src' }, false)
		).toBe(true);
		expect(
			isBlockingViolation({ blockedURI: 'https://cdn.x/a.js', effectiveDirective: 'script-src' }, true)
		).toBe(false);
	});

	it('a font / image block before init does not take the view down', () => {
		expect(
			isBlockingViolation({ blockedURI: 'https://fonts.x/a.woff2', effectiveDirective: 'font-src' }, false)
		).toBe(false);
	});

	it('a resource violation after init does not replace the view', () => {
		expect(
			isBlockingViolation({ blockedURI: 'https://img.x/a.png', effectiveDirective: 'img-src' }, true)
		).toBe(false);
	});
});

describe('blockedInfoFromViolation', () => {
	it('extracts the host from a URL', () => {
		expect(
			blockedInfoFromViolation({
				blockedURI: 'https://fal.media/files/abc',
				effectiveDirective: 'connect-src',
			})
		).toEqual({ host: 'fal.media', target: 'https://fal.media/files/abc', scope: 'csp:connect-src' });
	});

	it('keeps CSP keywords as-is', () => {
		expect(blockedInfoFromViolation({ blockedURI: 'eval', effectiveDirective: 'script-src' })).toEqual({
			host: 'eval',
			target: 'eval',
			scope: 'csp:script-src',
		});
	});

	it('treats an empty blockedURI as inline', () => {
		expect(blockedInfoFromViolation({ blockedURI: '', effectiveDirective: 'style-src' }).host).toBe(
			'inline'
		);
	});
});

describe('blockedInfoFromNavigation', () => {
	it('maps the Rust pkg://navigation-blocked payload', () => {
		expect(
			blockedInfoFromNavigation({
				pkgId: 'com.ikenga.studio',
				paneId: 'p1',
				url: 'https://fal.media/files/x',
				origin: 'https://fal.media',
				allowedOrigins: ['https://studio.example.com'],
			})
		).toEqual({
			host: 'fal.media',
			target: 'https://fal.media/files/x',
			scope: 'webview:allowed_origins',
		});
	});
});

describe('addedCapabilityLines', () => {
	it('lists only leaves the approved snapshot lacks', () => {
		const before = '{"capabilities":null,"permissions":{"fs":["read"]}}';
		const after = '{"capabilities":null,"permissions":{"fs":["read"],"net":["fal.media"]}}';
		expect(addedCapabilityLines(before, after)).toEqual(['permissions.net = fal.media']);
	});

	it('returns [] for unparseable input', () => {
		expect(addedCapabilityLines('{', '{"a":1}')).toEqual([]);
		expect(addedCapabilityLines('{}', 'nope')).toEqual([]);
	});

	it('treats an empty prior snapshot as nothing approved', () => {
		expect(addedCapabilityLines('', '{"permissions":{"fs":["write"]}}')).toEqual([
			'permissions.fs = write',
		]);
	});
});

describe('menu paths', () => {
	it('pkgIdFromRoutePath', () => {
		expect(pkgIdFromRoutePath('/pkg/com.ikenga.studio/grid')).toBe('com.ikenga.studio');
		expect(pkgIdFromRoutePath('/pkg/com.ikenga.studio')).toBe('com.ikenga.studio');
		expect(pkgIdFromRoutePath('/pkg/com.ikenga.studio?x=1')).toBe('com.ikenga.studio');
		expect(pkgIdFromRoutePath('/ngwa/installed')).toBeNull();
	});

	it('item detail + violation log paths', () => {
		expect(itemDetailPath('com.ikenga.studio')).toBe('/ngwa/item/com.ikenga.studio');
		expect(VIOLATION_LOG_PATH).toBe('/ngwa/health?section=violations');
	});
});
