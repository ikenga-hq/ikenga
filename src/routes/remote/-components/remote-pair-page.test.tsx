// D-16: an unexpected cookie-probe answer (e.g. a proxy 502) lets pairing
// proceed, but the page shows "couldn't confirm the pairing cookie" first.
// D-17: it then waits for the user's click; there is no auto-continue.

import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { PairOutcome } from './pair-flow';

const runPairingMock = vi.fn<(...args: unknown[]) => Promise<PairOutcome>>();
vi.mock('./pair-flow', async (orig) => ({
	...(await orig<typeof import('./pair-flow')>()),
	runPairing: (...args: unknown[]) => runPairingMock(...args),
}));

import { outcomeCopy, RemotePairPage } from './remote-pair-page';

afterEach(() => {
	cleanup();
	vi.useRealTimers();
	runPairingMock.mockReset();
});

async function submitCode() {
	fireEvent.change(screen.getByLabelText('Code'), { target: { value: 'K7P-42Q' } });
	await act(async () => {
		fireEvent.submit(screen.getByRole('form', { name: 'Pairing code' }));
	});
}

describe('<RemotePairPage /> cookie probe outcomes', () => {
	it('a confirmed pairing opens the workspace straight away', async () => {
		runPairingMock.mockResolvedValue({ kind: 'allowed', deviceId: 'd1', tier: 'dispatch' });
		const onPaired = vi.fn();
		render(<RemotePairPage onPaired={onPaired} />);
		await submitCode();
		expect(onPaired).toHaveBeenCalledTimes(1);
		expect(document.querySelector('[data-pair-warning]')).toBeNull();
	});

	it('an unconfirmed cookie shows the warning and waits for the user', async () => {
		vi.useFakeTimers();
		runPairingMock.mockResolvedValue({
			kind: 'allowed',
			deviceId: 'd1',
			tier: 'dispatch',
			cookieUnconfirmed: 'the check answered HTTP 502',
		});
		const onPaired = vi.fn();
		render(<RemotePairPage onPaired={onPaired} />);
		await submitCode();

		const warning = document.querySelector('[data-pair-warning="cookie-unconfirmed"]');
		expect(warning?.textContent).toMatch(/couldn't confirm the pairing cookie/i);
		expect(warning?.textContent).toContain('HTTP 502');
		expect(onPaired).not.toHaveBeenCalled();

		// Well past any former auto-continue delay: still waiting.
		act(() => {
			vi.advanceTimersByTime(60_000);
		});
		expect(onPaired).not.toHaveBeenCalled();
		expect(document.querySelector('[data-pair-warning="cookie-unconfirmed"]')).not.toBeNull();
		expect(warning?.textContent).not.toMatch(/opening your workspace anyway/i);

		fireEvent.click(screen.getByRole('button', { name: 'Open your workspace' }));
		expect(onPaired).toHaveBeenCalledTimes(1);
	});

	it('the warning offers to open the workspace now', async () => {
		runPairingMock.mockResolvedValue({
			kind: 'allowed',
			deviceId: 'd1',
			tier: 'dispatch',
			cookieUnconfirmed: "the check didn't reach the computer",
		});
		const onPaired = vi.fn();
		render(<RemotePairPage onPaired={onPaired} />);
		await submitCode();
		fireEvent.click(screen.getByRole('button', { name: 'Open your workspace' }));
		expect(onPaired).toHaveBeenCalledTimes(1);
	});

	it('plain allowed copy is unchanged', () => {
		expect(outcomeCopy({ kind: 'allowed', deviceId: 'd', tier: 't' }).title).toBe('Paired');
	});
});
