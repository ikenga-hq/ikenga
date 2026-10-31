import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, renderHook, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as tauriCmd from '@/lib/tauri-cmd';
import * as transport from '@/lib/transport';
import { useOutboxApprovalsActive } from './outbox-approvals-active';

let share: { role: 'operator' | 'reviewer' | 'guest' } | null = null;

vi.mock('@/shell/people/use-shared-projects', () => ({
	useCurrentShare: () => share,
}));
vi.mock('@/lib/tauri-cmd', () => ({
	pkgKernelStatus: vi.fn(),
	paActionsList: vi.fn(),
}));
vi.mock('@/lib/transport', () => ({
	invoke: vi.fn(),
	isBrowserSession: vi.fn(),
}));

const cmd = vi.mocked(tauriCmd);
const tr = vi.mocked(transport);

function wrapper({ children }: { children: ReactNode }) {
	const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return <QueryClientProvider client={qc}>{children}</QueryClientProvider>;
}

const kernel = (ids: string[], enabled = true) =>
	({
		api_version: 5,
		registries: {},
		installed: ids.map((id) => ({ id, enabled })),
	}) as never;

beforeEach(() => {
	share = null;
	cmd.pkgKernelStatus.mockResolvedValue(kernel([]));
	cmd.paActionsList.mockResolvedValue([]);
});
afterEach(() => {
	cleanup();
	vi.resetAllMocks();
});

describe('useOutboxApprovalsActive: box (browser tab)', () => {
	beforeEach(() => tr.isBrowserSession.mockReturnValue(true));

	it('is active when the sender socket is live', async () => {
		tr.invoke.mockResolvedValue({ active: true });
		const { result } = renderHook(() => useOutboxApprovalsActive(), {
			wrapper,
		});
		await waitFor(() => expect(result.current).toBe(true));
		expect(tr.invoke).toHaveBeenCalledWith('outbox_approvals_status');
		expect(cmd.pkgKernelStatus).not.toHaveBeenCalled();
	});

	it('is inactive when the socket is not live', async () => {
		tr.invoke.mockResolvedValue({ active: false });
		const { result } = renderHook(() => useOutboxApprovalsActive(), {
			wrapper,
		});
		await waitFor(() => expect(tr.invoke).toHaveBeenCalled());
		expect(result.current).toBe(false);
	});

	it('is inactive when the server lacks the arm (rejects)', async () => {
		tr.invoke.mockRejectedValue(new Error('not implemented'));
		const { result } = renderHook(() => useOutboxApprovalsActive(), {
			wrapper,
		});
		await waitFor(() => expect(tr.invoke).toHaveBeenCalled());
		expect(result.current).toBe(false);
	});

	it('inside a share: an operator sees it', async () => {
		tr.invoke.mockResolvedValue({ active: true });
		share = { role: 'operator' };
		const { result } = renderHook(() => useOutboxApprovalsActive(), {
			wrapper,
		});
		await waitFor(() => expect(result.current).toBe(true));
	});

	it.each(['reviewer', 'guest'] as const)('inside a share: a %s does not', async (role) => {
		// The backend says active; only the role gate can make this false. Wait
		// until the query has settled (invoke called and its promise flushed) so a
		// still-loading `false` cannot satisfy the assertion.
		tr.invoke.mockClear();
		tr.invoke.mockResolvedValue({ active: true });
		share = { role };
		const qc = new QueryClient({
			defaultOptions: { queries: { retry: false } },
		});
		const w = ({ children }: { children: ReactNode }) => (
			<QueryClientProvider client={qc}>{children}</QueryClientProvider>
		);
		const { result } = renderHook(() => useOutboxApprovalsActive(), {
			wrapper: w,
		});
		await waitFor(() => expect(tr.invoke).toHaveBeenCalledTimes(1));
		await waitFor(() =>
			expect(qc.getQueryState(['outbox', 'approvals-status'])?.status).toBe('success')
		);
		expect(qc.getQueryData(['outbox', 'approvals-status'])).toBe(true);
		expect(result.current).toBe(false);
	});
});

describe('useOutboxApprovalsActive: desktop', () => {
	beforeEach(() => tr.isBrowserSession.mockReturnValue(false));

	it('is active when com.ikenga.outbound is installed', async () => {
		cmd.pkgKernelStatus.mockResolvedValue(kernel(['com.ikenga.outbound']));
		const { result } = renderHook(() => useOutboxApprovalsActive(), {
			wrapper,
		});
		await waitFor(() => expect(result.current).toBe(true));
		expect(tr.invoke).not.toHaveBeenCalled();
	});

	it('is inactive when the outbound pkg is disabled and there are no drafts', async () => {
		cmd.pkgKernelStatus.mockResolvedValue(kernel(['com.ikenga.outbound'], false));
		const { result } = renderHook(() => useOutboxApprovalsActive(), {
			wrapper,
		});
		await waitFor(() => expect(cmd.paActionsList).toHaveBeenCalled());
		await waitFor(() => expect(cmd.pkgKernelStatus).toHaveBeenCalled());
		expect(result.current).toBe(false);
	});

	it('is active when there are pa_action_drafts rows even without the pkg', async () => {
		cmd.paActionsList.mockResolvedValue([{ id: 'd1' }] as never);
		const { result } = renderHook(() => useOutboxApprovalsActive(), {
			wrapper,
		});
		await waitFor(() => expect(result.current).toBe(true));
	});

	it('is inactive with neither signal', async () => {
		const { result } = renderHook(() => useOutboxApprovalsActive(), {
			wrapper,
		});
		await waitFor(() => expect(cmd.paActionsList).toHaveBeenCalled());
		expect(result.current).toBe(false);
	});
});
