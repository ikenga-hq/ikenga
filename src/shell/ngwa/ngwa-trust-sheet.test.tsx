import { render, screen, fireEvent, cleanup, waitFor } from '@testing-library/react';
import { describe, expect, it, vi, afterEach } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { NgwaTrustSheet, operatorPolicy } from './ngwa-trust-sheet';
import type { AccessStatus } from '@/lib/access/client';
import type { NgwaItem } from '@ikenga/contract';
import * as tauriCmd from '@/lib/tauri-cmd';

const access = vi.hoisted(() => ({ status: null as unknown }));

vi.mock('@/lib/tauri-cmd', () => ({
	pkgTrustGrant: vi.fn(),
	pkgTrustRevoke: vi.fn(),
	// `access_status` (G-ACCESS §5.8): T0 unless a test sets a T1 status.
	invoke: vi.fn((cmd: string) =>
		cmd === 'access_status' && access.status
			? Promise.resolve(access.status)
			: Promise.reject(new Error('store_unavailable: none'))
	),
}));

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	access.status = null;
});

function t1Status(over: Partial<AccessStatus> = {}): AccessStatus {
	return {
		tier: 't1',
		store: 'ok',
		principal: { principalId: 'p', username: 'ada', isAdmin: false },
		credential: { via: 'device', deviceId: 'd', tier: 'dispatch' },
		caps: ['files', 'sessions', 'dispatch'],
		adminStrength: false,
		publicUrl: null,
		sharingEnabled: true,
		share: null,
		...over,
	};
}

describe('NgwaTrustSheet remote split (G-ACCESS §5.8, G-62)', () => {
	it('the desktop (T0) has no operator-policy block', () => {
		expect(operatorPolicy({ id: 'com.ikenga.tasks' }, null)).toBeNull();
		expect(operatorPolicy({ id: 'com.ikenga.tasks' }, { ...t1Status(), tier: 't0' })).toBeNull();
	});

	it('kernel pkgs are operator-installed; vault items need install in this context', () => {
		expect(operatorPolicy({ id: 'com.ikenga.tasks' }, t1Status())?.line).toMatch(
			/Installed by the operator/
		);
		const vault = { id: 'skill:personal:grill' };
		expect(operatorPolicy(vault, t1Status())).toMatchObject({ canInstall: false });
		expect(operatorPolicy(vault, t1Status())?.line).toMatch(/View \+ dispatch/);
		const full = t1Status({
			caps: ['files', 'sessions', 'dispatch', 'approve', 'install', 'settings', 'secrets'],
		});
		expect(operatorPolicy(vault, full)).toMatchObject({ canInstall: true });
		const share = t1Status({
			caps: ['files', 'sessions', 'install'],
			share: {
				projectKey: 'o/royalti-co',
				projectName: 'royalti-co',
				ownerUsername: 'ned',
				role: 'operator',
				scope: 'project',
			},
		});
		expect(operatorPolicy(vault, share)?.line).toMatch(/project scope only/);
	});

	it('renders "Operator policy" above "Your trust" on a T1 server', async () => {
		access.status = t1Status();
		renderWithClient(
			<NgwaTrustSheet
				open={true}
				onOpenChange={vi.fn()}
				item={makeItem({ id: 'com.ikenga.tasks' })}
				mode="review"
			/>
		);
		await waitFor(() => expect(screen.getByText('Operator policy')).toBeTruthy());
		expect(screen.getByText('Your trust')).toBeTruthy();
		expect(screen.getByText(/Installed by the operator/)).toBeTruthy();
	});
});

function makeItem(partial: Partial<NgwaItem> = {}): NgwaItem {
	const id = partial.id ?? 'pkg-test';
	return {
		id,
		kind: 'app',
		name: 'pkg-test',
		display_name: 'Test Package',
		description: 'A package with sensitive permissions',
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
			state: 'needs_approval',
			signed: true,
			auto_trusted: false,
			review_pending: true,
			perms: {
				shell_execute: ['cargo build'],
				fs_write_outside_sandbox: ['/var/log'],
				net: ['api.github.com'],
				vault_keys: ['SECRET_TOKEN'],
			},
			last_granted_at_ms: null,
		},
		placements: [],
		usage: null,
		requires: [],
		required_by: [],
		owner_pkg_id: null,
		install_path: '/path/to/pkg',
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

describe('NgwaTrustSheet (WP-18 / D-07)', () => {
	it('renders review mode with declared sensitive permissions', () => {
		const item = makeItem();
		renderWithClient(
			<NgwaTrustSheet
				open={true}
				onOpenChange={vi.fn()}
				item={item}
				mode="review"
			/>
		);

		expect(screen.getByText(/Review permissions · Test Package/)).toBeDefined();
		expect(screen.getByText(/shell:exec · cargo build/)).toBeDefined();
		expect(screen.getByText(/fs:write · \/var\/log/)).toBeDefined();
		expect(screen.getByText(/net · api.github.com/)).toBeDefined();
		expect(screen.getByText(/vault · SECRET_TOKEN/)).toBeDefined();
		expect(screen.getByText('Grant permissions')).toBeDefined();
	});

	it('renders update diff mode with newly added permissions', () => {
		const item = makeItem();
		renderWithClient(
			<NgwaTrustSheet
				open={true}
				onOpenChange={vi.fn()}
				item={item}
				mode="update"
				priorVersion="1.1.0"
				addedPermissions={['net · api.stripe.com', 'shell:exec · curl']}
			/>
		);

		expect(screen.getByText(/Permission change · Test Package/)).toBeDefined();
		expect(screen.getByText(/Newly requested permissions \(2\)/)).toBeDefined();
		expect(screen.getByText('net · api.stripe.com')).toBeDefined();
		expect(screen.getByText('shell:exec · curl')).toBeDefined();
		expect(screen.getByText('Approve update')).toBeDefined();
	});

	it('renders violation mode with blocked scope and target', () => {
		const item = makeItem();
		renderWithClient(
			<NgwaTrustSheet
				open={true}
				onOpenChange={vi.fn()}
				item={item}
				mode="violation"
				violationScopeKind="shell.execute"
				violationTarget="rm -rf /"
			/>
		);

		expect(screen.getByText(/Security violation · Test Package/)).toBeDefined();
		expect(screen.getByText('shell.execute')).toBeDefined();
		expect(screen.getByText('rm -rf /')).toBeDefined();
	});

	it('triggers pkgTrustGrant on approval', async () => {
		const item = makeItem();
		const onApproved = vi.fn();
		const onOpenChange = vi.fn();
		(tauriCmd.pkgTrustGrant as any).mockResolvedValue();

		renderWithClient(
			<NgwaTrustSheet
				open={true}
				onOpenChange={onOpenChange}
				item={item}
				mode="review"
				onApproved={onApproved}
			/>
		);

		fireEvent.click(screen.getByText('Grant permissions'));

		await waitFor(() => {
			expect(tauriCmd.pkgTrustGrant).toHaveBeenCalledWith('pkg-test', '1.2.0');
			expect(onApproved).toHaveBeenCalled();
			expect(onOpenChange).toHaveBeenCalledWith(false);
		});
	});

	it('triggers pkgTrustRevoke when revoking granted trust', async () => {
		const item = makeItem({
			trust: {
				state: 'granted',
				signed: true,
				auto_trusted: false,
				review_pending: false,
				perms: null,
				last_granted_at_ms: 1000,
			},
		});
		const onDenied = vi.fn();
		const onOpenChange = vi.fn();
		(tauriCmd.pkgTrustRevoke as any).mockResolvedValue();

		renderWithClient(
			<NgwaTrustSheet
				open={true}
				onOpenChange={onOpenChange}
				item={item}
				mode="review"
				onDenied={onDenied}
			/>
		);

		fireEvent.click(screen.getByText('Revoke trust'));

		await waitFor(() => {
			expect(tauriCmd.pkgTrustRevoke).toHaveBeenCalledWith('pkg-test');
			expect(onDenied).toHaveBeenCalled();
			expect(onOpenChange).toHaveBeenCalledWith(false);
		});
	});
});

describe('NgwaTrustSheet — trust the server never evaluated (the headless daemon)', () => {
	const REASON = 'trust evaluation is not available on this server: no trust store';
	const unavailableTrust = (perms: NgwaItem['trust']['perms']) =>
		({
			state: 'not_applicable',
			signed: false,
			auto_trusted: false,
			review_pending: false,
			perms,
			last_granted_at_ms: null,
			unavailable: REASON,
		}) as NgwaItem['trust'];

	it('says so, lists only what the manifest declares, and disables Grant with the reason', () => {
		const item = makeItem({
			trust: unavailableTrust({
				shell_execute: ['git *'],
				fs_write_outside_sandbox: [],
				net: [],
				vault_keys: [],
			}),
		});
		const { container } = renderWithClient(
			<NgwaTrustSheet open={true} onOpenChange={vi.fn()} item={item} mode="review" />
		);
		const note = container.querySelector('[data-trust-unavailable-note]') as HTMLElement;
		expect(note.textContent).toContain('Not available on this server');
		expect(note.textContent).toContain(REASON);
		expect(note.textContent).toContain('manifest declares');
		expect(container.textContent).toContain('declared in the manifest — not evaluated (1)');
		expect(screen.getByText('shell:exec · git *')).toBeDefined();
		expect(container.textContent).not.toContain('No sensitive permissions requested');
		expect(container.textContent).not.toContain('standard process boundaries');
		expect(container.textContent).not.toContain('Approval grants these capabilities');

		const grant = container.querySelector<HTMLButtonElement>('[data-act="grant"]') as HTMLButtonElement;
		expect(grant.disabled).toBe(true);
		expect(grant.title).toMatch(/Not available on this server/);
		fireEvent.click(grant);
		expect(tauriCmd.pkgTrustGrant).not.toHaveBeenCalled();
		expect(screen.queryByText('Revoke trust')).toBeNull();
	});

	it('an unreadable manifest: declared permissions unknown — never "none requested"', () => {
		const item = makeItem({ trust: unavailableTrust(null) });
		const { container } = renderWithClient(
			<NgwaTrustSheet open={true} onOpenChange={vi.fn()} item={item} mode="review" />
		);
		expect(container.querySelector('[data-trust-unavailable-note]')?.textContent).toContain(
			'declared permissions are unknown'
		);
		expect(container.textContent).not.toContain('No sensitive permissions requested');
		expect(container.textContent).not.toMatch(/Declared sensitive capabilities \(0\)/);
		expect(container.querySelector<HTMLButtonElement>('[data-act="grant"]')?.disabled).toBe(true);
	});

	it('a manifest declaring nothing sensitive: says so, never "standard process boundaries"', () => {
		const item = makeItem({
			trust: unavailableTrust({ shell_execute: [], fs_write_outside_sandbox: [], net: [], vault_keys: [] }),
		});
		const { container } = renderWithClient(
			<NgwaTrustSheet open={true} onOpenChange={vi.fn()} item={item} mode="review" />
		);
		expect(container.textContent).toContain('The manifest declares no sensitive permissions.');
		expect(container.textContent).not.toContain('No sensitive permissions requested');
		expect(container.textContent).not.toContain('standard process boundaries');
		expect(container.querySelector<HTMLButtonElement>('[data-act="grant"]')?.disabled).toBe(true);
	});
});
