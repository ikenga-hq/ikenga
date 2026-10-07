// Gap audit rank 3 — onboarding's Engine step in a browser session: the
// offline-mode install can't run (the daemon serves no pkg install), so the
// control says so before the click; on the desktop it stays usable, and a
// failed install names its real cause rather than blaming the registry.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({ remote: false, kernelStatus: vi.fn() }));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	isRemoteWebSession: () => h.remote,
	pkgKernelStatus: h.kernelStatus,
}));

// engine-logo pulls in @lobehub/ui, which jsdom can't load.
vi.mock('@/shell/onboarding/engine-logo', () => ({ EngineLogo: () => null }));

import { EngineBody } from './engine-body';

function renderBody() {
	return render(
		<QueryClientProvider
			client={new QueryClient({ defaultOptions: { mutations: { retry: false } } })}
		>
			<EngineBody onContinue={() => {}} results={{}} refresh={() => {}} />
		</QueryClientProvider>
	);
}

afterEach(() => {
	cleanup();
	h.remote = false;
	h.kernelStatus.mockReset();
});

describe('EngineBody offline CTA (gap rank 3)', () => {
	it('is disabled and reads the honest reason in a browser session', () => {
		h.remote = true;
		renderBody();
		const cta = screen.getByTestId('agents-offline-cta') as HTMLButtonElement;
		expect(cta.disabled).toBe(true);
		expect(cta.textContent).toBe('Not available on this server yet');
	});

	it('stays usable on the desktop, and a failed install names the real cause', async () => {
		h.kernelStatus.mockRejectedValue(new Error('tarball integrity mismatch'));
		renderBody();
		const cta = screen.getByTestId('agents-offline-cta') as HTMLButtonElement;
		expect(cta.disabled).toBe(false);
		expect(cta.textContent).toBe('Continue offline');
		fireEvent.click(cta);
		const err = await waitFor(() => screen.getByTestId('agents-offline-error'));
		expect(err.textContent).toContain('tarball integrity mismatch');
		expect(err.textContent).not.toMatch(/reach the registry/i);
	});
});
