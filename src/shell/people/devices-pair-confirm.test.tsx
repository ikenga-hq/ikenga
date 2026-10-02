// WP-74b: D-05 `pair-confirm` (G-ACCESS §3.5, §3.6, P-9) and the pair
// sheet's phase model (§3.7).

import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { useState } from 'react';
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

import type { DevicesView } from './devices-model';
import { insecureCookieWarning } from './devices-model';
import { PairConfirm, PairConfirmOverlay, usePairWatch } from './devices-pair-confirm';
import { PairSheet, pairSheetPhase } from './devices-pair-sheet';

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
	usePairWatch.setState({
		watching: {},
		pending: [],
		revision: 0,
		lastPaired: null,
		notice: null,
	});
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

const view: DevicesView = {
	source: 'desktop',
	run: 'running',
	runNote: '',
	address: 'http://100.94.12.30:4000',
	host: '100.94.12.30',
	exposure: 'tailnet',
	tailnetAddress: '100.94.12.30',
	tokenPresent: true,
	tokenMasked: '••••••••',
	pid: 1,
	mode: 'persistent',
};

/** Review B1: the sheet and the confirm, mounted together as in the app. */
function Desktop() {
	const [open, setOpen] = useState(true);
	return (
		<>
			<PairSheet open={open} onOpenChange={setOpen} view={view} />
			<PairConfirmOverlay />
		</>
	);
}

describe('Pair sheet → confirm hand-off (review B1)', () => {
	const ticket = {
		pairingId: 'pid-1',
		code: 'K7P-42Q',
		expiresAt: Date.now() + 600_000,
		pairUrl: 'http://100.94.12.30:4000/remote/pair',
		qrPayload: 'http://100.94.12.30:4000/remote/pair#c=K7P42Q&h=s1',
		cookieSecure: true,
	};

	it('closes the sheet without cancelling; Pair device decides; the toast follows', async () => {
		mocks.accessPairBegin.mockResolvedValue(ticket);
		mocks.accessPairPending.mockResolvedValue([request]);
		mocks.accessPairDecide.mockResolvedValue({
			device: { deviceId: 'd1', name: 'Pixel 9 · Chrome' },
		});
		render(<Desktop />);
		const pair = await screen.findByRole('button', { name: 'Pair device' });
		await waitFor(() => expect(document.querySelector('[data-state="pair"]')).toBeNull());
		expect(document.body.style.pointerEvents).not.toBe('none');
		fireEvent.pointerDown(pair);
		fireEvent.click(pair);
		await waitFor(() =>
			expect(mocks.accessPairDecide).toHaveBeenCalledWith('pid-1', 'allow', 'dispatch')
		);
		expect(mocks.accessPairCancel).not.toHaveBeenCalled();
		expect(await screen.findByText('Paired Pixel 9 · Chrome · View + dispatch')).toBeTruthy();
	});

	it('Deny decides deny, never cancels, and toasts the dead code', async () => {
		mocks.accessPairBegin.mockResolvedValue(ticket);
		mocks.accessPairPending.mockResolvedValue([request]);
		mocks.accessPairDecide.mockResolvedValue({});
		render(<Desktop />);
		fireEvent.click(await screen.findByRole('button', { name: 'Deny' }));
		await waitFor(() =>
			expect(mocks.accessPairDecide).toHaveBeenCalledWith('pid-1', 'deny', undefined)
		);
		expect(mocks.accessPairCancel).not.toHaveBeenCalled();
		expect(await screen.findByText('Denied · the code is now dead')).toBeTruthy();
		expect(screen.queryByRole('button', { name: 'Pair device' })).toBeNull();
	});

	it('closing the sheet on an open code still cancels it', async () => {
		mocks.accessPairBegin.mockResolvedValue(ticket);
		mocks.accessPairPending.mockResolvedValue([]);
		mocks.accessPairCancel.mockResolvedValue({});
		render(<Desktop />);
		await screen.findByText('K7P-42Q');
		fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
		await waitFor(() => expect(mocks.accessPairCancel).toHaveBeenCalledWith('pid-1'));
	});

	it('review M1: warns when the pairing link is plain HTTP and the cookie is Secure', async () => {
		mocks.accessPairBegin.mockResolvedValue(ticket);
		mocks.accessPairPending.mockResolvedValue([]);
		render(<Desktop />);
		await screen.findByText('K7P-42Q');
		expect(document.querySelector('[data-pair-warning="insecure-cookie"]')).not.toBeNull();
		expect(insecureCookieWarning({ ...ticket, cookieSecure: false })).toBeNull();
		expect(
			insecureCookieWarning({ ...ticket, pairUrl: 'https://ned.tail1.ts.net/remote/pair' })
		).toBeNull();
		expect(insecureCookieWarning({ ...ticket, pairUrl: null })).toBeNull();
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
		expect(
			pairSheetPhase(ticket, null, new Set(['pid-1']), 1, new Map([['pid-1', 60_000]]))
		).toEqual({ kind: 'paused', ticket, retryAfterMs: 60_000 });
		expect(pairSheetPhase(ticket, { message: 'paused', paused: true }, new Set(), 1)).toEqual({
			kind: 'error',
			message: 'paused',
			paused: true,
		});
	});
});
