import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const calls = vi.hoisted(() => ({
	set: vi.fn(async () => {}),
	setScoped: vi.fn(async () => {}),
	del: vi.fn(async () => {}),
	delScoped: vi.fn(async () => {}),
}));

vi.mock('@/lib/tauri-cmd', () => ({
	secretsSet: calls.set,
	secretsSetScoped: calls.setScoped,
	secretsDelete: calls.del,
	secretsDeleteScoped: calls.delScoped,
	secretsQuarantined: vi.fn(async () => []),
}));
vi.mock('@/lib/transport/dialog-shim', () => ({ confirm: vi.fn(async () => true) }));

import { NeedsReentryGroup } from './needs-reentry';

const entries = [
	{ name: 'workspace::GOOGLE_KEY', scope: { kind: 'workspace' as const }, key: 'GOOGLE_KEY' },
	{ name: 'OLD_BARE', scope: null, key: 'OLD_BARE' },
];

function renderGroup(writable = true, list = entries) {
	const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return render(
		<QueryClientProvider client={qc}>
			<NeedsReentryGroup entries={list} writable={writable} />
		</QueryClientProvider>
	);
}

describe('NeedsReentryGroup', () => {
	beforeEach(() => vi.clearAllMocks());
	afterEach(cleanup);

	it('renders nothing when nothing needs re-entry', () => {
		const { container } = renderGroup(true, []);
		expect(container.firstChild).toBeNull();
	});

	it('lists names under a plain heading', () => {
		renderGroup();
		expect(screen.getByText('Needs re-entry')).toBeTruthy();
		expect(screen.getByText('GOOGLE_KEY')).toBeTruthy();
		expect(screen.getByText('OLD_BARE')).toBeTruthy();
	});

	it('saves a scoped entry through its scope', async () => {
		renderGroup();
		fireEvent.click(screen.getByLabelText('Enter GOOGLE_KEY again'));
		fireEvent.change(document.getElementById('reentry-value-input') as HTMLInputElement, {
			target: { value: 'new-value' },
		});
		fireEvent.click(screen.getByText('Save'));
		await waitFor(() =>
			expect(calls.setScoped).toHaveBeenCalledWith({ kind: 'workspace' }, 'GOOGLE_KEY', 'new-value')
		);
	});

	it('removes a bare entry through the unscoped command', async () => {
		renderGroup();
		fireEvent.click(screen.getByLabelText('Remove OLD_BARE'));
		await waitFor(() => expect(calls.del).toHaveBeenCalledWith('OLD_BARE'));
	});

	it('disables actions while the vault is locked', () => {
		renderGroup(false);
		expect((screen.getByLabelText('Enter GOOGLE_KEY again') as HTMLButtonElement).disabled).toBe(
			true
		);
	});
});
