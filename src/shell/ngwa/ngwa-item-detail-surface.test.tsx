import { render, screen, fireEvent, cleanup, waitFor } from '@testing-library/react';
import { describe, expect, it, vi, afterEach } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { NgwaItemDetailSurface } from './ngwa-item-detail-surface';
import type { NgwaItem } from '@ikenga/contract';
import * as tauriCmd from '@/lib/tauri-cmd';
import type { NgwaAct, NgwaItemActionSet } from '@/lib/ngwa/use-ngwa-actions';

vi.mock('@/lib/tauri-cmd', () => ({
	pkgSettingsGet: vi.fn(),
	pkgSettingsSet: vi.fn(),
	pkgTrustGrant: vi.fn(),
	pkgTrustRevoke: vi.fn(),
	// WP-31: the Flow tab reads `workflows[]` off the manifest. Default to a
	// manifest with none so the existing tab assertions are unaffected.
	pkgPreviewManifest: vi.fn().mockResolvedValue({ id: 'test-item', workflows: [] }),
}));

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

function makeItem(partial: Partial<NgwaItem>): NgwaItem {
	const id = partial.id ?? 'test-item';
	const name = partial.name ?? id;
	return {
		id,
		kind: 'app',
		name,
		display_name: partial.display_name ?? name,
		description: 'A test equipment package item',
		version: '1.2.0',
		latest_version: '1.3.0',
		scope: { kind: 'personal' },
		origin: {
			source: 'registry',
			url: null,
			ref: null,
			resolved_version: '1.2.0',
			publisher: 'ikenga-hq',
			managed: true,
			auto_update: false,
			installed_at_ms: 1000,
			updated_at_ms: 1000,
		},
		state: 'enabled',
		runtime: null,
		trust: {
			state: 'auto_trusted',
			signed: true,
			auto_trusted: true,
			review_pending: false,
			perms: {
				shell_execute: ['bun test'],
				fs_write_outside_sandbox: ['/tmp/artifacts'],
				net: ['api.example.com'],
				vault_keys: ['API_KEY'],
			},
			last_granted_at_ms: null,
		},
		placements: [
			{
				engine: 'claude',
				scope: { kind: 'personal' },
				path: '/home/.claude/pkgs/test-item',
				mechanism: 'symlink-dir',
				managed_by: 'pkg',
				status: 'active',
				format: 'md-yaml',
				present: true,
				link_target: null,
				in_store: true,
				overridden_by: null,
			},
		],
		usage: {
			source: 'transcript',
			window_start_ms: 0,
			count_7d: 12,
			count_30d: 45,
			tokens_30d: 125000,
			last_used_ms: Date.now() - 3600000,
		},
		requires: [
			{
				kind: 'skill',
				name: 'helper-skill',
				ref: null,
				source: 'registry',
				item_id: null,
			},
		],
		required_by: [],
		owner_pkg_id: null,
		install_path: '/home/.ikenga/pkgs/test-item',
		engines: ['claude'],
		...partial,
	};
}

function renderWithClient(ui: React.ReactElement) {
	const client = new QueryClient({
		defaultOptions: {
			queries: { retry: false },
		},
	});
	return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
}

function act(label: string, disabledReason?: string): NgwaAct {
	return { label, disabledReason, run: vi.fn() };
}

function stubActions(over: Partial<NgwaItemActionSet>): NgwaItemActionSet {
	const pick = { title: 'Move to', targets: [] };
	return {
		toggle: act('Disable'),
		move: pick,
		copy: { ...pick, title: 'Copy to' },
		moveToProject: act('Move to project'),
		moveToPersonal: act('Move to personal'),
		update: act('Update', '1.0.0 is the newest published version'),
		openFolder: act('Open folder'),
		remove: act('Remove…'),
		handToChi: act('Hand to Chi'),
		openView: null,
		openManifest: act('Open manifest.json'),
		revealInstallPath: act('Reveal install path'),
		resetSettings: act('Reset settings to defaults'),
		copyIyke: act('Copy as iyke'),
		iyke: 'iyke ngwa item test-item',
		briefChi: null,
		...over,
	};
}

describe('NgwaItemDetailSurface (WP-17 / D-08)', () => {
	it('renders package tabs correctly for app/tool/engine/sidecar', () => {
		const item = makeItem({ id: 'pkg-studio', kind: 'app', display_name: 'Ikenga Studio' });
		renderWithClient(<NgwaItemDetailSurface item={item} />);

		expect(screen.getByRole('tab', { name: 'Overview' })).toBeDefined();
		expect(screen.getByRole('tab', { name: 'Settings' })).toBeDefined();
		expect(screen.getByRole('tab', { name: 'Permissions' })).toBeDefined();
		expect(screen.getByRole('tab', { name: 'Files' })).toBeDefined();
		expect(screen.getByRole('tab', { name: 'Activity' })).toBeDefined();
		expect(screen.getByRole('tab', { name: 'Versions' })).toBeDefined();
		expect(screen.getByText('Ikenga Studio')).toBeDefined();
		expect(screen.getByText('v1.2.0')).toBeDefined();
	});

	it('renders primitive tabs correctly for skills, agents, commands', () => {
		const item = makeItem({
			id: 'skill-groundwork',
			kind: 'skill',
			display_name: 'groundwork',
			name: 'groundwork',
		});
		renderWithClient(<NgwaItemDetailSurface item={item} />);

		expect(screen.getByRole('tab', { name: 'Overview' })).toBeDefined();
		expect(screen.getByRole('tab', { name: 'SKILL.md' })).toBeDefined();
		expect(screen.getByRole('tab', { name: 'Usage' })).toBeDefined();
		expect(screen.getByRole('tab', { name: 'Scope' })).toBeDefined();
	});

	it('switches tabs and displays tab content', async () => {
		const item = makeItem({ id: 'pkg-studio', kind: 'app', display_name: 'Studio App' });
		(tauriCmd.pkgSettingsGet as any).mockResolvedValue({
			pkg_id: 'pkg-studio',
			schema: [
				{
					key: 'api_url',
					type: 'string',
					label: 'API Server URL',
					default: 'http://localhost:8080',
				},
				{
					key: 'enable_debug',
					type: 'bool',
					label: 'Enable Debug',
					default: false,
				},
			],
			values: {
				api_url: 'http://localhost:8080',
				enable_debug: true,
			},
		});

		renderWithClient(<NgwaItemDetailSurface item={item} />);

		// Switch to Settings tab
		fireEvent.click(screen.getByRole('tab', { name: 'Settings' }));
		await waitFor(() => {
			expect(screen.getByText('API Server URL')).toBeDefined();
			expect(screen.getByText('Enable Debug')).toBeDefined();
		});

		// Switch to Permissions tab
		fireEvent.click(screen.getByRole('tab', { name: 'Permissions' }));
		expect(screen.getByText(/shell:exec · bun test/)).toBeDefined();
		expect(screen.getByText(/fs:write · \/tmp\/artifacts/)).toBeDefined();

		// Switch to Activity tab
		fireEvent.click(screen.getByRole('tab', { name: 'Activity' }));
		expect(screen.getByText('12 sessions')).toBeDefined();
		expect(screen.getByText('45 sessions')).toBeDefined();

		// Switch to Versions tab
		fireEvent.click(screen.getByRole('tab', { name: 'Versions' }));
		expect(screen.getByText('Version status')).toBeDefined();
	});

	it('D-08 header: a pkg shows Open view · Disable · ⋯ and no Uninstall / Update / Hand to Chi chips', () => {
		const item = makeItem({
			id: 'pkg-studio',
			kind: 'app',
			state: 'update',
			latest_version: '1.3.0',
			origin: { ...makeItem({}).origin, source: 'registry' },
		});
		const actions = stubActions({ openView: act('Open view') });
		renderWithClient(<NgwaItemDetailSurface item={item} actions={actions} />);

		const header = document.querySelector<HTMLElement>('[data-idacts]') as HTMLElement;
		const labels = Array.from(header.querySelectorAll('button')).map(
			(b) => b.getAttribute('aria-label') ?? b.textContent?.trim()
		);
		expect(labels).toEqual(['Open view', 'Disable', 'More']);
		expect(screen.queryByText(/Uninstall/)).toBeNull();
		expect(screen.queryByText(/Update to/)).toBeNull();
		expect(screen.queryByText('Hand to Chi')).toBeNull();

		fireEvent.click(screen.getByRole('button', { name: /Disable/ }));
		expect(actions.toggle.run).toHaveBeenCalledTimes(1);
		fireEvent.click(screen.getByRole('button', { name: /Open view/ }));
		expect(actions.openView?.run).toHaveBeenCalledTimes(1);
	});

	it('D-08 header: a pkg with no views has no Open view; Enable when disabled', () => {
		const item = makeItem({ id: 'pkg-x', kind: 'tool', state: 'disabled' });
		renderWithClient(
			<NgwaItemDetailSurface item={item} actions={stubActions({ toggle: act('Enable') })} />
		);
		expect(screen.queryByRole('button', { name: /Open view/ })).toBeNull();
		expect(screen.getByRole('button', { name: /Enable/ })).toBeTruthy();
	});

	it('the ⋯ menu lists itemDotsMenu() with Reset as danger, and runs the picked action', () => {
		const item = makeItem({ id: 'pkg-studio', kind: 'app' });
		const actions = stubActions({});
		renderWithClient(<NgwaItemDetailSurface item={item} actions={actions} />);
		fireEvent.click(screen.getByRole('button', { name: 'More' }));
		const menu = screen.getByRole('menu', { name: 'More' });
		const items = Array.from(menu.querySelectorAll('[role="menuitem"]'));
		expect(items.map((b) => b.textContent)).toEqual([
			'Open manifest.json',
			'Reveal install path',
			'Reset settings to defaults',
			'Copy as iyke',
		]);
		expect(items[2].className).toContain('danger');
		fireEvent.click(items[1]);
		expect(actions.revealInstallPath.run).toHaveBeenCalledTimes(1);
		expect(screen.queryByRole('menu')).toBeNull();
	});

	it('D-08 skill variant: Brief a Chi · ⋯, no Disable', () => {
		const item = makeItem({ id: 'skill:personal:groundwork', kind: 'skill', name: 'groundwork' });
		const actions = stubActions({ briefChi: act('Brief a Chi') });
		renderWithClient(<NgwaItemDetailSurface item={item} actions={actions} />);
		const header = document.querySelector<HTMLElement>('[data-idacts]') as HTMLElement;
		const labels = Array.from(header.querySelectorAll('button')).map(
			(b) => b.getAttribute('aria-label') ?? b.textContent?.trim()
		);
		expect(labels).toEqual(['Brief a Chi', 'More']);
		fireEvent.click(screen.getByRole('button', { name: /Brief a Chi/ }));
		expect(actions.briefChi?.run).toHaveBeenCalledTimes(1);
	});

	it('the Versions tab updates through the shared Update action', () => {
		const item = makeItem({
			id: 'pkg-studio',
			kind: 'app',
			state: 'update',
			latest_version: '1.3.0',
		});
		const actions = stubActions({ update: act('Update to 1.3.0') });
		renderWithClient(<NgwaItemDetailSurface item={item} actions={actions} />);
		fireEvent.click(screen.getByRole('tab', { name: 'Versions' }));
		fireEvent.click(screen.getByText('Update to 1.3.0'));
		expect(actions.update.run).toHaveBeenCalledTimes(1);
	});

	// ── Flow tab (WP-31 review fix, Round 29) ───────────────────────────────

	it('mounts the flow renderer behind a Flow tab for a pkg declaring workflows[]', async () => {
		vi.mocked(tauriCmd.pkgPreviewManifest).mockResolvedValueOnce({
			id: 'com.ikenga.studio',
			name: 'Studio',
			version: '1.2.0',
			ikenga_api: '5',
			workflows: [
				{
					id: 'nightly',
					title: 'Nightly Build',
					steps: [
						{
							id: 'build',
							title: 'Build Artifacts',
							handler: '/iyke/pkg/com.ikenga.studio/build',
							inputs: {},
							produces: [],
							depends_on: [],
						},
						{
							id: 'publish',
							title: 'Publish Artifacts',
							handler: '/iyke/pkg/com.ikenga.studio/publish',
							inputs: {},
							produces: [],
							depends_on: ['build'],
						},
					],
				},
			],
		} as never);

		const item = makeItem({
			id: 'com.ikenga.studio',
			kind: 'app',
			install_path: '/home/.ikenga/pkgs/studio',
		});
		renderWithClient(<NgwaItemDetailSurface item={item} />);

		// The tab appears once the manifest resolves…
		const flowTab = await waitFor(() => screen.getByRole('tab', { name: 'Flow' }));
		fireEvent.click(flowTab);

		// …and the renderer is mounted with the graph `graph.ts` built.
		await waitFor(() => expect(screen.getByTestId('ngwa-flow-tab')).toBeDefined());
		// The graph title shows both as the group head and in the renderer.
		expect(screen.getAllByText('Nightly Build').length).toBeGreaterThan(0);
		expect(screen.getByText('Build Artifacts')).toBeDefined();
		expect(screen.getByText('Publish Artifacts')).toBeDefined();
	});

	it('offers no Flow tab when the manifest declares no workflows[]', async () => {
		const item = makeItem({ id: 'pkg-plain', kind: 'app' });
		renderWithClient(<NgwaItemDetailSurface item={item} />);

		await waitFor(() => expect(screen.getByRole('tab', { name: 'Overview' })).toBeDefined());
		expect(screen.queryByRole('tab', { name: 'Flow' })).toBeNull();
	});
});

describe('NgwaItemDetailSurface — trust the server never evaluated', () => {
	it('Permissions reads "Not available on this server": no unsigned/untrusted claim, no approve/revoke, no "sandboxed"', () => {
		const REASON = 'trust evaluation is not available on this server: no trust store';
		const item = makeItem({
			id: 'com.x.app',
			name: 'com.x.app',
			trust: {
				state: 'not_applicable',
				signed: false,
				auto_trusted: false,
				review_pending: false,
				perms: null,
				last_granted_at_ms: null,
				unavailable: REASON,
			} as NgwaItem['trust'],
		});
		const { container } = renderWithClient(<NgwaItemDetailSurface item={item} />);
		fireEvent.click(screen.getByRole('tab', { name: 'Permissions' }));
		const note = container.querySelector('[data-trust-unavailable]') as HTMLElement;
		expect(note.textContent).toContain('Not available on this server');
		expect(note.textContent).toContain(REASON);
		expect(container.querySelector('.badge.t-unsigned')).toBeNull();
		for (const badge of container.querySelectorAll('.badge')) {
			expect(badge.textContent).not.toMatch(/unsigned|untrusted/i);
		}
		expect(container.textContent).not.toContain('Runs fully sandboxed');
		expect(screen.queryByRole('button', { name: /Revoke trust|Approve permissions/ })).toBeNull();
		// No manifest perms: unknown, never "none requested".
		expect(note.textContent).toContain('declared permissions are unknown');
		expect(container.querySelector('[data-perms-heading]')).toBeNull();
	});

	it('Permissions shows what the manifest DECLARES, marked not evaluated, with no approve/revoke', () => {
		const REASON = 'trust evaluation is not available on this server: no trust store';
		const item = makeItem({
			id: 'com.x.app',
			name: 'com.x.app',
			trust: {
				state: 'not_applicable',
				signed: false,
				auto_trusted: false,
				review_pending: false,
				perms: {
					shell_execute: ['git *'],
					fs_write_outside_sandbox: ['$home/out/**'],
					net: [],
					vault_keys: [],
				},
				last_granted_at_ms: null,
				unavailable: REASON,
			} as NgwaItem['trust'],
		});
		const { container } = renderWithClient(<NgwaItemDetailSurface item={item} />);
		fireEvent.click(screen.getByRole('tab', { name: 'Permissions' }));
		expect(container.querySelector('[data-trust-unavailable]')?.textContent).toContain(
			'nothing here evaluated or approved it'
		);
		expect(container.querySelector('[data-perms-heading]')?.textContent).toBe(
			'Sensitive permissions declared in the manifest — not evaluated (2)'
		);
		const list = container.querySelector('[data-perms-declared]') as HTMLElement;
		expect(list.textContent).toContain('shell:exec · git *');
		expect(list.textContent).toContain('fs:write · $home/out/**');
		expect(container.textContent).not.toContain('Runs fully sandboxed');
		expect(screen.queryByRole('button', { name: /Revoke trust|Approve permissions/ })).toBeNull();
	});
});
