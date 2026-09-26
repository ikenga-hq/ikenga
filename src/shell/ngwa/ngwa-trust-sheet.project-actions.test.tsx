import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { afterEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
	actionsTrustStatus: vi.fn(),
	readActionsFiles: vi.fn(),
	actionsTrustGrant: vi.fn(),
}));

vi.mock('@/lib/actions/client', () => ({
	actionsTrustStatus: mocks.actionsTrustStatus,
	readActionsFiles: mocks.readActionsFiles,
	actionsTrustGrant: mocks.actionsTrustGrant,
	watchActionsFiles: () => Promise.resolve(() => {}),
}));

vi.mock('@/lib/tauri-cmd', () => ({
	pkgTrustGrant: vi.fn(),
	pkgTrustRevoke: vi.fn(),
}));

import type { ActionsTrustStatus, KeybindingRule } from '@/lib/actions/types';
import { bindingsHash } from '@/lib/actions/runner/trust';
import { bindingLine, NgwaTrustSheet } from './ngwa-trust-sheet';

const BINDINGS: KeybindingRule[] = [
	{ key: 'enter', command: 'delete', when: 'filesFocus' },
	{ key: 'mod+w', command: '-pane.close' },
];

async function trustStatus(bindingsState: 'untrusted' | 'trusted' = 'untrusted'): Promise<ActionsTrustStatus> {
	return {
		projectId: 'p1',
		projectRoot: '/work/royalti-pulse',
		actions: [
			{
				id: 'refresh-pulse',
				name: 'Refresh pulse snapshots',
				kind: 'shell',
				run: { kind: 'shell', command: 'scripts/pulse/build-all.sh', cwd: '{{project.root}}', confirm: true },
				hash: 'h-refresh',
				state: 'untrusted',
			},
			{
				id: 'ask',
				name: 'Ask',
				kind: 'chi',
				run: { kind: 'chi', target: 'new', prompt: 'hi' },
				hash: 'h-ask',
				state: 'not-gated',
			},
		],
		actionsError: null,
		actionsStale: false,
		keybindings: { hash: await bindingsHash(BINDINGS), ruleCount: 2, state: bindingsState },
		keybindingsError: null,
		keybindingsStale: false,
	};
}

function files(bindings: KeybindingRule[]) {
	return {
		project: { keybindings: { document: { version: 1, bindings } } },
	};
}

function renderSheet(onApproved = vi.fn()) {
	const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	render(
		<QueryClientProvider client={client}>
			<NgwaTrustSheet
				open
				onOpenChange={() => {}}
				item={null}
				mode="project-actions"
				projectActions={{ projectId: 'p1', projectName: 'royalti-pulse', actionIds: ['refresh-pulse'] }}
				onApproved={onApproved}
			/>
		</QueryClientProvider>
	);
	return onApproved;
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('NgwaTrustSheet — project-actions mode (WP-53)', () => {
	it('lists untrusted actions with their exact run text, and the held keybindings', async () => {
		mocks.actionsTrustStatus.mockResolvedValue(await trustStatus());
		mocks.readActionsFiles.mockResolvedValue(files(BINDINGS));
		renderSheet();
		expect(await screen.findByText('Trust project actions · royalti-pulse')).toBeTruthy();
		await waitFor(() => expect(screen.getAllByText(/scripts\/pulse\/build-all\.sh/).length).toBeGreaterThan(0));
		// Ungated (chi) actions are not listed.
		expect(screen.queryByText(/Ask ·/)).toBeNull();
		expect(screen.getByText(/Keybindings held until trusted \(2\)/)).toBeTruthy();
		expect(screen.getByText(bindingLine(BINDINGS[0]))).toBeTruthy();
		expect(screen.getByText('mod+w → unbind pane.close')).toBeTruthy();
	});

	it('Trust pins the shown action hashes and the keybindings hash (DEC-65)', async () => {
		const status = await trustStatus();
		mocks.actionsTrustStatus.mockResolvedValue(status);
		mocks.readActionsFiles.mockResolvedValue(files(BINDINGS));
		mocks.actionsTrustGrant.mockResolvedValue(status);
		const onApproved = renderSheet();
		await screen.findByText(/Keybindings held until trusted/);
		// Held keybindings are opt-in: tick them.
		fireEvent.click(screen.getByRole('checkbox', { name: 'Trust these keybindings' }));
		fireEvent.click(screen.getByRole('button', { name: /Trust$/ }));
		await waitFor(() => expect(onApproved).toHaveBeenCalled());
		expect(mocks.actionsTrustGrant).toHaveBeenCalledWith(
			{ actions: [{ id: 'refresh-pulse', hash: 'h-refresh' }], keybindings: status.keybindings.hash },
			'p1'
		);
	});

	it('pre-checks only the refused action; other actions and held keybindings start unchecked', async () => {
		const status = await trustStatus();
		status.actions.push({
			id: 'deploy',
			name: 'Deploy',
			kind: 'shell',
			run: { kind: 'shell', command: 'scripts/deploy.sh' },
			hash: 'h-deploy',
			state: 'untrusted',
		});
		mocks.actionsTrustStatus.mockResolvedValue(status);
		mocks.readActionsFiles.mockResolvedValue(files(BINDINGS));
		mocks.actionsTrustGrant.mockResolvedValue(status);
		renderSheet();
		await screen.findByText(/Keybindings held until trusted/);
		expect((screen.getByRole('checkbox', { name: 'Trust Refresh pulse snapshots' }) as HTMLInputElement).checked).toBe(
			true
		);
		expect((screen.getByRole('checkbox', { name: 'Trust Deploy' }) as HTMLInputElement).checked).toBe(false);
		expect((screen.getByRole('checkbox', { name: 'Trust these keybindings' }) as HTMLInputElement).checked).toBe(false);
		fireEvent.click(screen.getByRole('button', { name: /Trust$/ }));
		await waitFor(() => expect(mocks.actionsTrustGrant).toHaveBeenCalled());
		expect(mocks.actionsTrustGrant).toHaveBeenCalledWith(
			{ actions: [{ id: 'refresh-pulse', hash: 'h-refresh' }], keybindings: null },
			'p1'
		);
	});

	it('does not trust keybindings that moved while the sheet loaded', async () => {
		const status = await trustStatus();
		mocks.actionsTrustStatus.mockResolvedValue(status);
		mocks.readActionsFiles.mockResolvedValue(files([{ key: 'mod+q', command: 'delete' }]));
		mocks.actionsTrustGrant.mockResolvedValue(status);
		renderSheet();
		await screen.findByText(/changed while this sheet loaded/);
		fireEvent.click(screen.getByRole('button', { name: /Trust$/ }));
		await waitFor(() => expect(mocks.actionsTrustGrant).toHaveBeenCalled());
		expect(mocks.actionsTrustGrant.mock.calls[0][0].keybindings).toBeNull();
	});

	it('a trusted keybindings file shows no held section', async () => {
		mocks.actionsTrustStatus.mockResolvedValue(await trustStatus('trusted'));
		mocks.readActionsFiles.mockResolvedValue(files(BINDINGS));
		renderSheet();
		await screen.findByText(/Actions waiting for trust \(1\)/);
		expect(screen.queryByText(/Keybindings held until trusted/)).toBeNull();
	});
});
