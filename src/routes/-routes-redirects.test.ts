// Tests for legacy route redirects and search params/hash preservation (WP-10).

import { describe, expect, it } from 'vitest';
import { Route as PackagesRoute } from './packages';
import { Route as PackagesBrowseRoute } from './packages_.browse';
import { Route as InstallRoute } from './install';
import { Route as ClaudeRoute } from './claude/route';
import { Route as CronRoute } from './cron';
import { Route as AgentRunsRoute } from './agent-runs';
import { Route as ProjectIndexRoute } from './project/index';
import { Route as NgwaIndexRoute } from './ngwa/index';

describe('WP-10 Route Redirects', () => {
	it('redirects /packages to /ngwa/installed preserving search and hash', () => {
		const beforeLoad = PackagesRoute.options.beforeLoad;
		expect(beforeLoad).toBeDefined();

		try {
			beforeLoad!({
				search: { filter: 'installed' },
				location: { hash: '#catalog' } as any,
			} as any);
			expect.unreachable('should have thrown redirect');
		} catch (err: any) {
			expect(err.options.to).toBe('/ngwa/installed');
			expect(err.options.search).toEqual({ filter: 'installed' });
			expect(err.options.hash).toBe('#catalog');
		}
	});

	it('redirects /packages?filter=store to /ngwa/store', () => {
		const beforeLoad = PackagesRoute.options.beforeLoad;
		try {
			beforeLoad!({
				search: { filter: 'store' },
				location: { hash: '' } as any,
			} as any);
			expect.unreachable('should have thrown redirect');
		} catch (err: any) {
			expect(err.options.to).toBe('/ngwa/store');
			expect(err.options.search).toEqual({ filter: 'store' });
		}
	});

	it('redirects /packages_/browse to /ngwa/store preserving search and hash', () => {
		const beforeLoad = PackagesBrowseRoute.options.beforeLoad;
		expect(beforeLoad).toBeDefined();

		try {
			beforeLoad!({
				search: { query: 'terminal' },
				location: { hash: '#top' } as any,
			} as any);
			expect.unreachable('should have thrown redirect');
		} catch (err: any) {
			expect(err.options.to).toBe('/ngwa/store');
			expect(err.options.search).toEqual({ query: 'terminal' });
			expect(err.options.hash).toBe('#top');
		}
	});

	it('redirects /install to /ngwa/installed defaulting install to local-path', () => {
		const beforeLoad = InstallRoute.options.beforeLoad;
		expect(beforeLoad).toBeDefined();

		try {
			beforeLoad!({
				search: {},
				location: { hash: '#step1' } as any,
			} as any);
			expect.unreachable('should have thrown redirect');
		} catch (err: any) {
			expect(err.options.to).toBe('/ngwa/installed');
			expect(err.options.search).toEqual({ install: 'local-path' });
			expect(err.options.hash).toBe('#step1');
		}
	});

	it('redirects /install with explicit install param preserved', () => {
		const beforeLoad = InstallRoute.options.beforeLoad;
		try {
			beforeLoad!({
				search: { install: 'manifest-url' },
				location: { hash: '' } as any,
			} as any);
			expect.unreachable('should have thrown redirect');
		} catch (err: any) {
			expect(err.options.to).toBe('/ngwa/installed');
			expect(err.options.search).toEqual({ install: 'manifest-url' });
		}
	});

	it('redirects /claude to /ngwa/installed with mapped search params and hash', () => {
		const beforeLoad = ClaudeRoute.options.beforeLoad;
		expect(beforeLoad).toBeDefined();

		try {
			beforeLoad!({
				search: {
					surface: 'graph',
					scope: 'personal',
					kind: 'agents',
					sys: 'claude,gemini',
				},
				location: { hash: '#inspector' } as any,
			} as any);
			expect.unreachable('should have thrown redirect');
		} catch (err: any) {
			expect(err.options.to).toBe('/ngwa/installed');
			expect(err.options.search).toEqual({
				surface: 'graph',
				scope: 'personal',
				kind: 'agents',
				sys: 'claude,gemini',
			});
			expect(err.options.hash).toBe('#inspector');
		}
	});

	it('redirects /cron to /automations preserving search and hash', () => {
		const beforeLoad = CronRoute.options.beforeLoad;
		expect(beforeLoad).toBeDefined();

		try {
			beforeLoad!({
				search: { filter: 'active' },
				location: { hash: '#cron-table' } as any,
			} as any);
			expect.unreachable('should have thrown redirect');
		} catch (err: any) {
			expect(err.options.to).toBe('/automations');
			expect(err.options.search).toEqual({ filter: 'active' });
			expect(err.options.hash).toBe('#cron-table');
		}
	});

	it('redirects /agent-runs to /automations (view=runs) preserving search and hash', () => {
		const beforeLoad = AgentRunsRoute.options.beforeLoad;
		expect(beforeLoad).toBeDefined();

		try {
			beforeLoad!({
				search: { id: 'run-123' },
				location: { hash: '#log' } as any,
			} as any);
			expect.unreachable('should have thrown redirect');
		} catch (err: any) {
			expect(err.options.to).toBe('/automations');
			expect(err.options.search).toEqual({ id: 'run-123', view: 'runs' });
			expect(err.options.hash).toBe('#log');
		}
	});

	it('redirects /project/ to /project/dashboard preserving hash', () => {
		const beforeLoad = ProjectIndexRoute.options.beforeLoad;
		expect(beforeLoad).toBeDefined();

		try {
			beforeLoad!({
				search: {},
				location: { hash: '#widgets' } as any,
			} as any);
			expect.unreachable('should have thrown redirect');
		} catch (err: any) {
			expect(err.options.to).toBe('/project/dashboard');
			expect(err.options.hash).toBe('#widgets');
		}
	});

	it('redirects /ngwa/ to /ngwa/installed preserving search and hash', () => {
		const beforeLoad = NgwaIndexRoute.options.beforeLoad;
		expect(beforeLoad).toBeDefined();

		try {
			beforeLoad!({
				search: { kind: 'skills' },
				location: { hash: '#list' } as any,
			} as any);
			expect.unreachable('should have thrown redirect');
		} catch (err: any) {
			expect(err.options.to).toBe('/ngwa/installed');
			expect(err.options.search).toEqual({ kind: 'skills' });
			expect(err.options.hash).toBe('#list');
		}
	});
});
