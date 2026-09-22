import { render, screen, fireEvent, cleanup, waitFor } from '@testing-library/react';
import { describe, expect, it, vi, afterEach } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { NgwaTrustSheet } from './ngwa-trust-sheet';
import type { NgwaItem } from '@ikenga/contract';
import * as tauriCmd from '@/lib/tauri-cmd';

vi.mock('@/lib/tauri-cmd', () => ({
	pkgTrustGrant: vi.fn(),
	pkgTrustRevoke: vi.fn(),
}));

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
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
