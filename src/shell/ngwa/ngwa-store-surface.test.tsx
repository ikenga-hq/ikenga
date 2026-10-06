// Ngwa Store Surface component tests (WP-15 / locked D-02).

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, fireEvent, cleanup, waitFor, within } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { NgwaStoreSurface, type NgwaStoreSurfaceProps } from './ngwa-store-surface';
import { NeedsApprovalError, type StoreUpdateOptions } from '@/lib/ngwa/use-store-install';
import { useUpdateApprovals } from '@/lib/ngwa/use-update-approvals';
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
		expect(install.title).toBe('Tick every consent above first (0 of 2 ticked)');
		// The reason is visible in the foot, not only a tooltip, with a live count.
		const foot = document.querySelector<HTMLElement>('[data-sheetfoot]')!;
		const reason = () => foot.querySelector('[data-install-blocked]')?.textContent ?? null;
		expect(reason()).toBe('Tick every consent above first (0 of 2 ticked)');
		expect(install.getAttribute('aria-describedby')).toBe(
			foot.querySelector('[data-install-blocked]')?.id
		);

		fireEvent.click(boxes[0]);
		expect(install.disabled).toBe(true);
		expect(reason()).toBe('Tick every consent above first (1 of 2 ticked)');
		fireEvent.click(boxes[1]);
		expect(install.disabled).toBe(false);
		expect(reason()).toBeNull();

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

	it('DEC-71: with the Default project active, a pkg installs to personal and the menu lists no Default target', async () => {
		const onInstall = vi.fn();
		const loadDetail = vi.fn().mockResolvedValue(detailVersion({}));
		renderWithClient(
			<NgwaStoreSurface
				catalog={mockCatalog}
				loadDetail={loadDetail}
				onInstall={onInstall}
				activeProjectName="Default"
				activeProjectId="default"
			/>
		);
		selectRow('skill-groundwork');
		await screen.findByText(/declares intent and never grants itself anything/);

		expect(screen.queryByRole('button', { name: 'Install to Default' })).toBeNull();
		const install = screen.getByRole('button', { name: 'Install to personal' });
		fireEvent.click(install);
		expect(onInstall).toHaveBeenLastCalledWith(mockCatalog[1], 'personal');

		fireEvent.click(screen.getByLabelText('Choose install scope'));
		const items = screen.getAllByRole('menuitem');
		expect(items).toHaveLength(1);
		expect(items[0].textContent).toContain('Install to personal');
		expect(items[0].textContent).not.toContain('Default');
	});

	it('keeps a real project as the pkg install target', async () => {
		const loadDetail = vi.fn().mockResolvedValue(detailVersion({}));
		renderWithClient(
			<NgwaStoreSurface
				catalog={mockCatalog}
				loadDetail={loadDetail}
				onInstall={vi.fn()}
				activeProjectName="Kinnect"
				activeProjectId="kinnect"
			/>
		);
		selectRow('skill-groundwork');
		await screen.findByText(/declares intent and never grants itself anything/);
		expect(screen.getByRole('button', { name: 'Install to Kinnect' })).toBeDefined();
		fireEvent.click(screen.getByLabelText('Choose install scope'));
		expect(screen.getAllByRole('menuitem')).toHaveLength(2);
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

	it('turns Install into a progress row for the real promise, then a readable failure with Retry', async () => {
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

		fireEvent.click(screen.getByRole('button', { name: 'Install to active project' }));
		expect(onInstall).toHaveBeenCalledWith(mockCatalog[1], 'project');
		const foot = container.querySelector('[data-sheetfoot]') as HTMLElement;
		// The button is replaced by a step label over an indeterminate bar.
		expect(within(foot).getByRole('status').textContent).toContain('Installing');
		expect(within(foot).getByRole('progressbar')).toBeDefined();
		expect(within(foot).queryByRole('button', { name: 'Install to active project' })).toBeNull();
		expect(
			container.querySelector('.srow[data-id="skill-groundwork"]')?.getAttribute('aria-busy')
		).toBe('true');

		reject(new Error('npm warn tar TAR_ENTRY_ERROR ENOSPC: no space left on device, write'));
		const alert = await within(foot).findByRole('alert');
		expect(alert.textContent).toMatch(/^Not enough disk space to install .+\. Free some space and try again\.$/);
		expect(within(foot).queryByRole('progressbar')).toBeNull();

		// The raw log sits behind Show details.
		expect(within(foot).queryByText(/TAR_ENTRY_ERROR/)).toBeNull();
		fireEvent.click(within(foot).getByRole('button', { name: 'Show details' }));
		expect(within(foot).getByText(/TAR_ENTRY_ERROR/)).toBeDefined();

		// Retry repeats the same install.
		fireEvent.click(within(foot).getByRole('button', { name: 'Retry' }));
		expect(onInstall).toHaveBeenCalledTimes(2);
		expect(onInstall).toHaveBeenLastCalledWith(mockCatalog[1], 'project');
	});

	it('row Update opens the sheet, shows Updating while in flight, and surfaces a failure', async () => {
		let reject: (e: Error) => void = () => {};
		const onUpdate = vi.fn(
			() =>
				new Promise<void>((_, rej) => {
					reject = rej;
				})
		);
		const { container } = renderWithClient(
			<NgwaStoreSurface catalog={mockCatalog} onUpdate={onUpdate} />
		);

		fireEvent.click(screen.getByRole('button', { name: /^update$/i }));
		expect(onUpdate).toHaveBeenCalledWith(mockCatalog[0]);
		const foot = container.querySelector('[data-sheetfoot]') as HTMLElement;
		expect(within(foot).getByRole('status').textContent).toContain('Updating');
		expect(within(foot).queryByRole('button', { name: 'Update 0.8.2 → 0.8.3' })).toBeNull();

		reject(new Error('npm error code ECONNRESET'));
		expect((await within(foot).findByRole('alert')).textContent).toMatch(
			/Couldn't reach the package registry/
		);
		expect(within(foot).getByRole('button', { name: 'Retry' })).toBeDefined();
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

describe('an installed pkg that failed to load (Bug 3)', () => {
	const meetings: NgwaStoreEntry = {
		id: '@ikenga/pkg-meetings',
		name: '@ikenga/pkg-meetings',
		displayName: 'pkg-meetings',
		description: 'Meetings',
		version: '0.2.1',
		latestVersion: '0.2.1',
		kind: 'app',
		trustFacet: 'unsigned',
		installedItem: null,
		isUpdate: false,
		registryEntry: {
			name: '@ikenga/pkg-meetings',
			latest: '0.2.1',
			detail: 'https://example.com/meetings.json',
		},
		broken: 'on disk but failed to load: `ui.nav` was removed in manifest v5',
	};

	it('the row says installed but broken, and its action reads Reinstall, not Install', () => {
		renderWithClient(<NgwaStoreSurface catalog={[meetings]} onInstall={vi.fn()} />);
		const row = document.querySelector('[data-id="@ikenga/pkg-meetings"]') as HTMLElement;
		expect(within(row).getByText(/installed · failed to load/)).toBeDefined();
		expect(within(row).getByRole('button', { name: 'Reinstall' })).toBeDefined();
		expect(within(row).queryByRole('button', { name: 'Install' })).toBeNull();
	});

	it('Reinstall opens the sheet and runs the normal consent-gated install', async () => {
		const onInstall = vi.fn();
		const loadDetail = vi.fn().mockResolvedValue(studioLike);
		renderWithClient(
			<NgwaStoreSurface
				catalog={[meetings]}
				loadDetail={loadDetail}
				onInstall={onInstall}
				activeProjectName="royalti-co"
			/>
		);
		const row = document.querySelector('[data-id="@ikenga/pkg-meetings"]') as HTMLElement;
		fireEvent.click(within(row).getByRole('button', { name: 'Reinstall' }));
		// Opening the sheet never installs by itself.
		expect(onInstall).not.toHaveBeenCalled();

		expect(await screen.findByText('Share kola')).toBeDefined();
		expect(document.querySelector('[data-broken-note]')?.textContent).toMatch(/failed to load/);
		const reinstall = screen.getByRole('button', {
			name: 'Reinstall to royalti-co',
		}) as HTMLButtonElement;
		expect(reinstall.disabled).toBe(true);
		for (const box of screen.getAllByRole('checkbox')) fireEvent.click(box);
		expect(reinstall.disabled).toBe(false);
		fireEvent.click(reinstall);
		expect(onInstall).toHaveBeenCalledWith(meetings, 'project');
	});

	it('initialSelectedId opens that row sheet (Health → Reinstall from registry)', async () => {
		const loadDetail = vi.fn().mockResolvedValue(detailVersion({}));
		renderWithClient(
			<NgwaStoreSurface
				catalog={[meetings]}
				loadDetail={loadDetail}
				onInstall={vi.fn()}
				initialSelectedId="@ikenga/pkg-meetings"
			/>
		);
		expect(
			await screen.findByRole('button', { name: 'Reinstall to active project' })
		).toBeDefined();
	});
});

// ─── Updates held for approval (WP-41-F1 in the Store) ───────────────────────

const TASKS_REVIEW = {
	pkg_id: '@ikenga/pkg-tasks',
	manifest_version: '0.8.3',
	old_capabilities: '{}',
	new_capabilities: '{"net":["https://api.example.com"]}',
	prior_approved_at_ms: 0,
};

/** The Store route's wiring: the surface plus the shared approvals hook,
 *  whose modal is mounted once beside it. */
function StoreWithApprovals({
	update,
	...props
}: Omit<NgwaStoreSurfaceProps, 'updateApprovals' | 'onUpdate'> & {
	update: (entry: NgwaStoreEntry, opts?: StoreUpdateOptions) => Promise<void>;
}) {
	const approvals = useUpdateApprovals({ update });
	return (
		<>
			<NgwaStoreSurface {...props} onUpdate={update} updateApprovals={approvals} />
			{approvals.element}
		</>
	);
}

/** Held unless re-run approved. */
function heldUpdate() {
	return vi.fn((entry: NgwaStoreEntry, opts?: StoreUpdateOptions) =>
		opts?.approved
			? Promise.resolve()
			: Promise.reject(new NeedsApprovalError([{ entry, review: TASKS_REVIEW }]))
	);
}

describe('NgwaStoreSurface — updates that need approval', () => {
	it('a held row Update opens the trust review instead of failing; Approve runs it approved', async () => {
		const update = heldUpdate();
		renderWithClient(<StoreWithApprovals catalog={mockCatalog} update={update} />);

		fireEvent.click(screen.getByRole('button', { name: /^update$/i }));
		const row = await screen.findByTestId('trust-review-row-@ikenga/pkg-tasks');
		expect(screen.getByText('Capability review')).toBeDefined();
		expect(screen.queryByText(/^Failed:/)).toBeNull();

		fireEvent.click(within(row).getByTestId('trust-review-approve-@ikenga/pkg-tasks'));
		await waitFor(() => expect(update).toHaveBeenCalledTimes(2));
		expect(update.mock.calls[1]).toEqual([mockCatalog[0], { approved: true }]);
		await waitFor(() => expect(screen.queryByText('Capability review')).toBeNull());
		expect(document.querySelector('[data-update-approvals]')).toBeNull();
	});

	it('Reject drops it: nothing is re-run and the strip clears', async () => {
		const update = heldUpdate();
		renderWithClient(<StoreWithApprovals catalog={mockCatalog} update={update} />);

		fireEvent.click(screen.getByRole('button', { name: /^update$/i }));
		await screen.findByTestId('trust-review-row-@ikenga/pkg-tasks');
		fireEvent.click(screen.getByTestId('trust-review-reject-@ikenga/pkg-tasks'));
		await waitFor(() => expect(screen.queryByText('Capability review')).toBeNull());
		expect(update).toHaveBeenCalledTimes(1);
		expect(document.querySelector('[data-update-approvals]')).toBeNull();
	});

	it('Update all puts approvals on the strip ("needs approval · Review") apart from real failures', async () => {
		const update = heldUpdate();
		const other: NgwaStoreEntry = {
			...mockCatalog[0],
			id: '@ikenga/pkg-notes',
			name: '@ikenga/pkg-notes',
			displayName: 'pkg-notes',
		};
		const catalog = [mockCatalog[0], other, mockCatalog[1]];
		const onUpdateAll = vi
			.fn()
			.mockRejectedValue(
				new NeedsApprovalError(
					[{ entry: mockCatalog[0], review: TASKS_REVIEW }],
					['pkg-notes: 404']
				)
			);
		renderWithClient(
			<StoreWithApprovals catalog={catalog} update={update} onUpdateAll={onUpdateAll} />
		);

		fireEvent.click(screen.getByRole('button', { name: 'Update all (2)' }));
		fireEvent.click(
			within(screen.getByRole('dialog')).getByRole('button', { name: 'Update all (2)' })
		);

		// The review opens for the held row…
		await screen.findByTestId('trust-review-row-@ikenga/pkg-tasks');
		// …the strip says it needs approval, and shows the real failure apart.
		const strip = document.querySelector<HTMLElement>('[data-updates]')!;
		expect(strip.querySelector('[data-update-approvals]')?.textContent).toContain(
			'1 needs approval'
		);
		expect(strip.querySelector('[data-update-failures]')?.textContent).toBe(
			'1 of 2 failed — pkg-notes: 404'
		);
		expect(strip.querySelector('[data-update-failures]')?.textContent).not.toMatch(/tasks/);

		// Closing the modal keeps the hold; Review reopens it.
		fireEvent.click(
			screen.getAllByRole('button', { name: 'Close' }).find((b) => b.textContent === 'Close')!
		);
		await waitFor(() => expect(screen.queryByText('Capability review')).toBeNull());
		fireEvent.click(within(strip).getByRole('button', { name: 'Review' }));
		expect(await screen.findByText('Capability review')).toBeDefined();
	});
});
