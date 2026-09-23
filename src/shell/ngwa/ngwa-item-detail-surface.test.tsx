import { render, screen, fireEvent, cleanup, waitFor } from '@testing-library/react';
import { describe, expect, it, vi, afterEach } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { NgwaItemDetailSurface } from './ngwa-item-detail-surface';
import type { NgwaItem } from '@ikenga/contract';
import * as tauriCmd from '@/lib/tauri-cmd';

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

	it('triggers toggle state and hand to chi actions', () => {
		const item = makeItem({
			id: 'pkg-studio',
			kind: 'app',
			state: 'enabled',
		});
		const onToggle = vi.fn();
		const onHandToChi = vi.fn();

		renderWithClient(
			<NgwaItemDetailSurface
				item={item}
				onToggleState={onToggle}
				onHandToChi={onHandToChi}
			/>
		);

		fireEvent.click(screen.getByText('Disable'));
		expect(onToggle).toHaveBeenCalledWith(item);

		fireEvent.click(screen.getByText('Hand to Chi'));
		expect(onHandToChi).toHaveBeenCalledWith(item);
	});

	it('triggers update action when update is available', () => {
		const item = makeItem({
			id: 'pkg-studio',
			kind: 'app',
			state: 'update',
			latest_version: '1.3.0',
		});
		const onUpdate = vi.fn();

		renderWithClient(
			<NgwaItemDetailSurface
				item={item}
				onUpdate={onUpdate}
			/>
		);

		fireEvent.click(screen.getByText('Update to 1.3.0'));
		expect(onUpdate).toHaveBeenCalledWith(item);
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
