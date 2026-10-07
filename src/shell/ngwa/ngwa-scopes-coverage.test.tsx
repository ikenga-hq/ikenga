// A project the config scan did not read (on the headless daemon: its root
// is outside the fs allowlist) reads "not scanned" in Scopes — never an empty
// column — while the columns that WERE read stay knowable and actionable.

import { afterEach, describe, expect, it } from 'vitest';
import { cleanup, render } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { mkItem, mkPlacement } from '@/routes/ngwa/-ngwa-test-fixtures';
import { NgwaScopesSurface, type NgwaScopeActions } from './ngwa-scopes-surface';

afterEach(cleanup);

const noop = () => Promise.resolve();
const actions: NgwaScopeActions = {
	enable: noop,
	disable: noop,
	copy: noop,
	move: noop,
	remove: noop,
	enableFor: noop,
	disableFor: noop,
	pkgSetEnabled: noop,
	pkgUninstall: noop,
	openStore: () => {},
};

const SCOPES = [
	{ key: 'personal', label: 'Personal', sub: '~/.claude', active: false },
	{ key: 'project:far', label: 'far', sub: '.claude', active: false, root: '/x/far' },
	{ key: 'project:near', label: 'near', sub: '.claude', active: false, root: '/x/near' },
];

const PARTIAL =
	'partially unreadable — project roots not scanned: `/x/far` (path outside allowlist: /x/far)';

function mount(error: string) {
	const items = [
		mkItem({
			id: 'skill:personal:tidy',
			kind: 'skill',
			name: 'tidy',
			placements: [mkPlacement({ path: '/home/u/.claude/skills/tidy' })],
			engines: ['claude'],
		}),
	];
	const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return render(
		<QueryClientProvider client={qc}>
			<NgwaScopesSurface
				items={items}
				scopes={SCOPES}
				homeDir="/home/u"
				actions={actions}
				unreadableSources={[{ source: 'engine_config', error, unavailable: false }]}
			/>
		</QueryClientProvider>
	);
}

const mark = (c: HTMLElement, cell: string) =>
	c.querySelector(`[data-cell="${cell}"]`)?.getAttribute('data-mark');

describe('Scopes — a project the config scan did not read', () => {
	it('its column reads "not scanned" with the reason; read columns stay known', () => {
		const { container } = mount(PARTIAL);
		const far = container.querySelector('th[data-col="project:far"]') as HTMLElement;
		expect(far.hasAttribute('data-notscanned')).toBe(true);
		expect(far.querySelector('.colsub')?.textContent).toBe('not scanned');
		expect(far.title).toMatch(/outside this server's allowlist/);
		expect(far.querySelector<HTMLButtonElement>('[data-enall="project:far"]')?.disabled).toBe(true);

		const near = container.querySelector('th[data-col="project:near"]') as HTMLElement;
		expect(near.hasAttribute('data-notscanned')).toBe(false);
		expect(near.querySelector('.colsub')?.textContent).toBe('.claude');

		expect(mark(container, 'prim:skill:tidy|project:far')).toBe('unknown');
		expect(mark(container, 'prim:skill:tidy|project:near')).toBe('none');
		expect(mark(container, 'prim:skill:tidy|personal')).not.toBe('unknown');
		// Partial, not failed: the whole matrix is not made unknown.
		expect(container.querySelector('[data-conflicts-unknown]')).toBeNull();
		// Still said out loud, as a partial read.
		expect(container.querySelector('[data-unreadable]')?.textContent).toContain(
			'partially unreadable'
		);
	});

	it('a scan that failed outright still makes every primitive cell unknown', () => {
		const { container } = mount('HOME unset');
		expect(mark(container, 'prim:skill:tidy|personal')).toBe('unknown');
		expect(mark(container, 'prim:skill:tidy|project:near')).toBe('unknown');
		expect(container.querySelector('[data-notscanned]')).toBeNull();
		expect(container.querySelector('[data-conflicts-unknown]')).not.toBeNull();
	});
});
