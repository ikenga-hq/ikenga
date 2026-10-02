// WP-74b: D-05 `pair-confirm` (G-ACCESS §3.5, §3.6, P-9) and the pair
// sheet's phase model (§3.7).

import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
	accessPairDecide: vi.fn(),
	accessPairPending: vi.fn(),
	accessPairBegin: vi.fn(),
	accessPairCancel: vi.fn(),
}));

vi.mock('@/lib/access/client', async (orig) => ({
	...(await orig<typeof import('@/lib/access/client')>()),
	...mocks,
}));

import type { PairRequest, PairTicket } from '@/lib/access/client';

import { PairConfirm, usePairWatch } from './devices-pair-confirm';
import { pairSheetPhase } from './devices-pair-sheet';

const request: PairRequest = {
	pairingId: 'pid-1',
	deviceName: 'Pixel 9 · Chrome',
	platform: 'android',
	remoteAddr: '100.94.12.31',
	askedAt: 1_000,
	code: 'K7P-42Q',
	fingerprint: ['graph', 'colossal', 'bacon', 'whinny'],
	state: 'awaiting_host',
};

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('PairConfirm', () => {
	it('shows the request, the code and the four words; defaults to View + dispatch', () => {
		render(<PairConfirm request={request} now={13_000} />);
		expect(screen.getByText('Pixel 9 · Chrome')).toBeTruthy();
		expect(screen.getByText('100.94.12.31')).toBeTruthy();
		expect(screen.getByText('12 s ago')).toBeTruthy();
		expect(screen.getByText('K7P-42Q')).toBeTruthy();
		expect(screen.getByText('graph · colossal · bacon · whinny')).toBeTruthy();
		expect(screen.queryByText('Full')).toBeNull();
		expect(
			screen.getByText('Everything above, plus sending instructions to a session.')
		).toBeTruthy();
	});

	it('Pair device sends the chosen tier; Deny sends deny', async () => {
		usePairWatch.setState({ watching: { 'pid-1': Date.now() + 60_000 }, pending: [request] });
		mocks.accessPairDecide.mockResolvedValue({ device: { deviceId: 'd1' } });
		render(<PairConfirm request={request} now={13_000} />);
		fireEvent.click(screen.getByRole('tab', { name: 'Dispatch + approve' }));
		fireEvent.click(screen.getByRole('button', { name: 'Pair device' }));
		await waitFor(() =>
			expect(mocks.accessPairDecide).toHaveBeenCalledWith('pid-1', 'allow', 'approve')
		);
		await waitFor(() => expect(usePairWatch.getState().watching['pid-1']).toBeUndefined());
		cleanup();
		mocks.accessPairDecide.mockResolvedValue({});
		render(<PairConfirm request={request} now={13_000} />);
		fireEvent.click(screen.getByRole('button', { name: 'Deny' }));
		await waitFor(() =>
			expect(mocks.accessPairDecide).toHaveBeenCalledWith('pid-1', 'deny', undefined)
		);
	});
});

describe('pairSheetPhase', () => {
	const ticket: PairTicket = {
		pairingId: 'pid-1',
		code: 'K7P-42Q',
		expiresAt: 600_000,
		pairUrl: null,
		qrPayload: null,
	};
	it('loading → code → burned / expired; errors win', () => {
		expect(pairSheetPhase(null, null, new Set(), 0).kind).toBe('loading');
		expect(pairSheetPhase(ticket, null, new Set(), 1).kind).toBe('code');
		expect(pairSheetPhase(ticket, null, new Set(['pid-1']), 1).kind).toBe('burned');
		expect(pairSheetPhase(ticket, null, new Set(), 600_000).kind).toBe('expired');
		expect(pairSheetPhase(ticket, { message: 'paused', paused: true }, new Set(), 1)).toEqual({
			kind: 'error',
			message: 'paused',
			paused: true,
		});
	});
});
