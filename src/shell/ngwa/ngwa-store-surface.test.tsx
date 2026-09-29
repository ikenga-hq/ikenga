// Ngwa Store Surface component tests (WP-15 / locked D-02).

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, fireEvent, cleanup, waitFor, within } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { NgwaStoreSurface } from './ngwa-store-surface';
import type { NgwaStoreEntry } from '@/lib/ngwa/enrichment';
import type { StorePkgVersion } from '@/lib/registry/client';

afterEach(() => {
	cleanup();
});

function renderWithClient(ui: React.ReactElement) {
	const client = new QueryClient({
		defaultOptions: { queries: { retry: false } },
	});
	return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
}

const mockCatalog: NgwaStoreEntry[] = [
	{
		id: '@ikenga/pkg-tasks',
		name: '@ikenga/pkg-tasks',
		displayName: 'pkg-tasks',
		description: 'Task management package',
		version: '0.8.2',
		latestVersion: '0.8.3',
		kind: 'app',
		trustFacet: 'signed',
		installedItem: {
			id: '@ikenga/pkg-tasks',
			name: '@ikenga/pkg-tasks',
			display_name: 'pkg-tasks',
			description: 'Task management package',
			version: '0.8.2',
			latest_version: '0.8.3',
			kind: 'app',
			scope: { kind: 'personal' },
			origin: {
				source: 'registry',
				url: 'https://registry.ikenga.dev/index.json',
				ref: null,
				resolved_version: '0.8.2',
				publisher: 'ikenga-hq',
				managed: true,
				auto_update: false,
				installed_at_ms: 1_726_000_000_000,
				updated_at_ms: 1_726_900_000_000,
			},
			state: 'update',
			runtime: null,
			trust: {
				state: 'granted',
				signed: true,
				auto_trusted: false,
				review_pending: false,
				perms: null,
				last_granted_at_ms: 1_726_900_000_000,
			},
			placements: [],
			usage: {
				source: 'transcript',
				last_used_ms: 1_726_950_000_000,
				count_7d: 5,
				count_30d: 20,
				tokens_30d: 150,
				window_start_ms: 1_724_000_000_000,
			},
			requires: [],
			required_by: [],
			owner_pkg_id: null,
			install_path: '/Users/x/.ikenga/pkgs/pkg-tasks',
			engines: ['claude'],
		},
		isUpdate: true,
		registryEntry: {
			name: '@ikenga/pkg-tasks',
			latest: '0.8.3',
			detail: 'https://example.com/tasks.json',
		},
	},
	{
		id: 'skill-groundwork',
		name: 'skill-groundwork',
		displayName: 'skill-groundwork',
		description: 'Foundational prompt skill',
		version: '1.0.0',
		latestVersion: '1.0.0',
		kind: 'skill',
		trustFacet: 'signed',
		installedItem: null,
		isUpdate: false,
		registryEntry: {
			name: 'skill-groundwork',
			latest: '1.0.0',
			detail: 'https://example.com/gw.json',
		},
	},
];

/** A registry detail-file version, as `fetchPkgVersionForStore` returns it. */
function detailVersion(
	manifest: Record<string, unknown>,
	extra: Record<string, unknown> = {}
): StorePkgVersion {
	return {
		version: '1.0.0',
		publishedAt: '2026-09-01T00:00:00Z',
		tarball: 'https://registry.npmjs.org/x/-/x-1.0.0.tgz',
		integrity: 'sha512-abc',
		size: 2_500_000,
		deps: [],
		screenshots: [],
		...extra,
		manifest: {
			id: 'com.ikenga.groundwork',
			name: 'groundwork',
			version: '1.0.0',
			ikenga_api: '1',
			author: { name: 'Royalti', key: 'royalti' },
			mcp: [],
			sidecars: [],
			requires: [],
			permissions: {},
			ui: {},
			...manifest,
		},
	} as unknown as StorePkgVersion;
}

const studioLike = detailVersion({
	requires: [
		{ kind: 'bundle', name: 'studio-archetypes', source: 'npx' },
		{ kind: 'skill', name: 'studio-doctor' },
	],
	mcp: [{ name: 'studio', command: 'bun', args: [], env: {} }],
	permissions: {
		'shell.execute': ['bun', 'ffmpeg'],
		net: ['https://esm.sh', 'http://127.0.0.1:*'],
	},
});

function selectRow(name: string) {
	fireEvent.click(screen.getByText(name));
}

describe('NgwaStoreSurface', () => {
	it('renders store catalog items', () => {
		const { container } = renderWithClient(<NgwaStoreSurface catalog={mockCatalog} />);
		expect(screen.getByText('@ikenga/pkg-tasks')).toBeDefined();
		expect(screen.getByText('skill-groundwork')).toBeDefined();
		expect(container.querySelector('.updates')).not.toBeNull();
	});

	it('renders updates banner with correct update count and details', () => {
		renderWithClient(<NgwaStoreSurface catalog={mockCatalog} />);
		expect(screen.getByText(/1 update available/i)).toBeDefined();
		expect(screen.getAllByText(/0.8.2 → 0.8.3/i).length).toBeGreaterThanOrEqual(1);
	});

	it('filters entries by search query', () => {
		renderWithClient(<NgwaStoreSurface catalog={mockCatalog} />);
		const searchInput = screen.getByPlaceholderText('Search the registry…');
		fireEvent.change(searchInput, { target: { value: 'groundwork' } });

		expect(screen.getByText('skill-groundwork')).toBeDefined();
		expect(screen.queryByText('@ikenga/pkg-tasks')).toBeNull();
		expect(screen.getByText(/1 of 2 shown/)).toBeDefined();
	});

	it('filters entries by kind facet chip', () => {
		const { container } = renderWithClient(<NgwaStoreSurface catalog={mockCatalog} />);
		const skillChip = container.querySelector('button[data-kind="skill"]') as HTMLElement;
		fireEvent.click(skillChip);

		expect(screen.getByText('skill-groundwork')).toBeDefined();
		expect(screen.queryByText('@ikenga/pkg-tasks')).toBeNull();
	});

	it('shows the empty sheet until a row is picked, and closes back to it', () => {
		renderWithClient(<NgwaStoreSurface catalog={mockCatalog} />);
		expect(
			screen.getByText(/Pick a row to read its closure, its permissions and its settings/)
		).toBeDefined();

		selectRow('skill-groundwork');
		expect(screen.queryByText(/Pick a row to read its closure/)).toBeNull();

		fireEvent.click(screen.getByLabelText('Close sheet'));
		expect(screen.getByText(/Pick a row to read its closure/)).toBeDefined();
	});

	it('row Update calls onUpdate; row Install opens the sheet instead of installing', () => {
		const onUpdate = vi.fn();
		const onInstall = vi.fn();
		renderWithClient(
			<NgwaStoreSurface catalog={mockCatalog} onUpdate={onUpdate} onInstall={onInstall} />
		);

		fireEvent.click(screen.getByRole('button', { name: /^update$/i }));
		expect(onUpdate).toHaveBeenCalledWith(mockCatalog[0]);

		fireEvent.click(screen.getByRole('button', { name: /^install$/i }));
		expect(onInstall).not.toHaveBeenCalled();
		expect(screen.getByRole('region', { name: 'Install sheet' }).textContent).toContain(
			'skill-groundwork'
		);
	});

	it('rows say "not read" until the detail file is fetched, then show closure and asks', async () => {
		const loadDetail = vi.fn().mockResolvedValue(studioLike);
		const { container } = renderWithClient(
			<NgwaStoreSurface catalog={mockCatalog} loadDetail={loadDetail} />
		);
		const row = container.querySelector('.srow[data-id="skill-groundwork"]') as HTMLElement;
		expect(row.querySelector('[data-closure]')?.textContent).toBe('closure not read');
		expect(row.querySelector('[data-asks]')?.textContent).toBe('permissions not read');
		// Nothing is fetched for rows nobody opened.
		expect(loadDetail).not.toHaveBeenCalled();

		selectRow('skill-groundwork');
		await waitFor(() =>
			expect(row.querySelector('[data-closure]')?.textContent).toBe(
				'also installs 1 bundle · 1 skill · 1 MCP server'
			)
		);
		expect(row.querySelector('[data-asks]')?.textContent).toBe(
			'asks: shell.execute · net (2 hosts)'
		);
		expect(loadDetail).toHaveBeenCalledTimes(1);
		expect(loadDetail.mock.calls[0][0]).toBe(mockCatalog[1]);
		// The other row was never read.
		const tasks = container.querySelector('.srow[data-id="@ikenga/pkg-tasks"]') as HTMLElement;
		expect(tasks.querySelector('[data-asks]')?.textContent).toBe('permissions not read');
	});

	it('fetches the detail for the selected row and renders the requires closure', async () => {
		const loadDetail = vi.fn().mockResolvedValue(studioLike);
		renderWithClient(
			<NgwaStoreSurface
				catalog={mockCatalog}
				loadDetail={loadDetail}
				activeProjectName="royalti-co"
			/>
		);
		selectRow('skill-groundwork');

		expect(await screen.findByText('Requires — the closure, before you consent')).toBeDefined();
		const sheet = screen.getByRole('region', { name: 'Install sheet' });
		const requires = sheet.querySelector('[data-requires]') as HTMLElement;
		expect(within(requires).getByText('studio-archetypes')).toBeDefined();
		expect(within(requires).getByText('studio-doctor')).toBeDefined();
		expect(requires.textContent).toContain('royalti-co · npx');
		expect(requires.textContent).toContain('royalti-co · catalog');
		// Header: publisher · pkg id · ikenga_api; footer: size; trust.
		expect(sheet.textContent).toContain('Royalti');
		expect(sheet.textContent).toContain('com.ikenga.groundwork');
		expect(sheet.textContent).toContain('2.4 MB');
		expect(sheet.textContent).toContain('absent — this manifest carries no ed25519 signature');
	});

	it('gates Install on every consent box, then installs to the active project', async () => {
		const onInstall = vi.fn();
		const loadDetail = vi.fn().mockResolvedValue(studioLike);
		renderWithClient(
			<NgwaStoreSurface
				catalog={mockCatalog}
				loadDetail={loadDetail}
				onInstall={onInstall}
				activeProjectName="royalti-co"
			/>
		);
		selectRow('skill-groundwork');

		expect(await screen.findByText('Share kola')).toBeDefined();
		const install = screen.getByRole('button', {
			name: 'Install to royalti-co',
		}) as HTMLButtonElement;
		const boxes = screen.getAllByRole('checkbox') as HTMLInputElement[];
		expect(boxes).toHaveLength(2);
		expect(install.disabled).toBe(true);
		expect(install.title).toBe('Tick every consent above first');

		fireEvent.click(boxes[0]);
		expect(install.disabled).toBe(true);
		fireEvent.click(boxes[1]);
		expect(install.disabled).toBe(false);

		fireEvent.click(install);
		expect(onInstall).toHaveBeenCalledWith(mockCatalog[1], 'project');
	});

	it('enables Install at once when the manifest asks for nothing', async () => {
		const loadDetail = vi.fn().mockResolvedValue(detailVersion({}));
		renderWithClient(
			<NgwaStoreSurface catalog={mockCatalog} loadDetail={loadDetail} onInstall={vi.fn()} />
		);
		selectRow('skill-groundwork');

		expect(
			await screen.findByText(/declares intent and never grants itself anything/)
		).toBeDefined();
		expect(screen.queryAllByRole('checkbox')).toHaveLength(0);
		const install = screen.getByRole('button', {
			name: 'Install to active project',
		}) as HTMLButtonElement;
		expect(install.disabled).toBe(false);
	});

	it('keeps Install disabled while the manifest is unread', () => {
		renderWithClient(<NgwaStoreSurface catalog={mockCatalog} onInstall={vi.fn()} />);
		selectRow('skill-groundwork');
		const install = screen.getByRole('button', { name: /^Install to/ }) as HTMLButtonElement;
		expect(install.disabled).toBe(true);
		expect(screen.getByText(/Closure and permissions not read/)).toBeDefined();
	});

	it('surfaces a detail-fetch failure with a retry', async () => {
		const loadDetail = vi
			.fn()
			.mockRejectedValueOnce(new Error('HTTP 404'))
			.mockResolvedValueOnce(studioLike);
		const { container } = renderWithClient(
			<NgwaStoreSurface catalog={mockCatalog} loadDetail={loadDetail} onInstall={vi.fn()} />
		);
		selectRow('skill-groundwork');

		expect(await screen.findByText(/read the manifest/)).toBeDefined();
		expect(container.querySelector('[data-state="ngwa-store-detail-error"]')).not.toBeNull();
		fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
		expect(await screen.findByText('Share kola')).toBeDefined();
	});

	it('renders the install-scope menu as a styled popover that Escape and outside clicks dismiss', async () => {
		const onInstall = vi.fn();
		const loadDetail = vi.fn().mockResolvedValue(detailVersion({}));
		renderWithClient(
			<NgwaStoreSurface catalog={mockCatalog} loadDetail={loadDetail} onInstall={onInstall} />
		);
		selectRow('skill-groundwork');
		await screen.findByText(/declares intent and never grants itself anything/);

		const chevronBtn = screen.getByLabelText('Choose install scope');
		fireEvent.click(chevronBtn);
		const menu = screen.getByRole('menu', { name: 'Install scope' });
		expect(menu.classList.contains('cellpop')).toBe(true);
		expect(menu.classList.contains('storepop')).toBe(true);
		expect(screen.getAllByRole('menuitem')).toHaveLength(2);
		expect(chevronBtn.getAttribute('aria-expanded')).toBe('true');

		fireEvent.keyDown(document, { key: 'Escape' });
		expect(screen.queryByRole('menu', { name: 'Install scope' })).toBeNull();

		fireEvent.click(chevronBtn);
		expect(screen.getByRole('menu', { name: 'Install scope' })).toBeDefined();
		fireEvent.mouseDown(document.body);
		expect(screen.queryByRole('menu', { name: 'Install scope' })).toBeNull();

		fireEvent.click(chevronBtn);
		fireEvent.click(screen.getByRole('menuitem', { name: /Install to personal/ }));
		expect(onInstall).toHaveBeenCalledWith(mockCatalog[1], 'personal');
	});

	it('Update all opens a review of each update and applies only on confirm', () => {
		const onUpdateAll = vi.fn();
		renderWithClient(<NgwaStoreSurface catalog={mockCatalog} onUpdateAll={onUpdateAll} />);

		fireEvent.click(screen.getByRole('button', { name: 'Update all (1)' }));
		const dialog = screen.getByRole('dialog');
		expect(within(dialog).getByText('Review 1 update')).toBeDefined();
		expect(within(dialog).getByText('@ikenga/pkg-tasks')).toBeDefined();
		expect(onUpdateAll).not.toHaveBeenCalled();

		fireEvent.click(within(dialog).getByRole('button', { name: 'Update all (1)' }));
		expect(onUpdateAll).toHaveBeenCalledWith([mockCatalog[0]]);
	});

	it('shows Registering for the real install promise and its failure in the foot', async () => {
		let reject: (e: Error) => void = () => {};
		const onInstall = vi.fn(
			() =>
				new Promise<void>((_, rej) => {
					reject = rej;
				})
		);
		const loadDetail = vi.fn().mockResolvedValue(detailVersion({}));
		const { container } = renderWithClient(
			<NgwaStoreSurface catalog={mockCatalog} loadDetail={loadDetail} onInstall={onInstall} />
		);
		selectRow('skill-groundwork');
		await screen.findByText(/declares intent and never grants itself anything/);

		const install = screen.getByRole('button', {
			name: 'Install to active project',
		}) as HTMLButtonElement;
		fireEvent.click(install);
		expect(onInstall).toHaveBeenCalledWith(mockCatalog[1], 'project');
		expect(screen.getByRole('status').textContent).toContain('Registering');
		expect(install.disabled).toBe(true);
		expect(
			container.querySelector('.srow[data-id="skill-groundwork"]')?.getAttribute('aria-busy')
		).toBe('true');

		reject(new Error('integrity mismatch'));
		expect(await screen.findByText('Failed: integrity mismatch')).toBeDefined();
		expect(screen.queryByText('Registering')).toBeNull();
		expect(install.disabled).toBe(false);
	});

	it('row Update opens the sheet, shows Updating while in flight, and surfaces a failure', async () => {
		let reject: (e: Error) => void = () => {};
		const onUpdate = vi.fn(
			() =>
				new Promise<void>((_, rej) => {
					reject = rej;
				})
		);
		renderWithClient(<NgwaStoreSurface catalog={mockCatalog} onUpdate={onUpdate} />);

		fireEvent.click(screen.getByRole('button', { name: /^update$/i }));
		expect(onUpdate).toHaveBeenCalledWith(mockCatalog[0]);
		expect(screen.getByRole('status').textContent).toContain('Updating');
		expect(
			(screen.getByRole('button', { name: 'Update 0.8.2 → 0.8.3' }) as HTMLButtonElement).disabled
		).toBe(true);

		reject(new Error('asks for new permissions'));
		expect(await screen.findByText('Failed: asks for new permissions')).toBeDefined();
	});

	it('Update all shows its failure on the strip', async () => {
		const onUpdateAll = vi
			.fn()
			.mockRejectedValue(new Error('1 of 1 update failed — pkg-tasks: 404'));
		renderWithClient(<NgwaStoreSurface catalog={mockCatalog} onUpdateAll={onUpdateAll} />);

		fireEvent.click(screen.getByRole('button', { name: 'Update all (1)' }));
		fireEvent.click(
			within(screen.getByRole('dialog')).getByRole('button', { name: 'Update all (1)' })
		);
		expect(await screen.findByText('1 of 1 update failed — pkg-tasks: 404')).toBeDefined();
	});
});
