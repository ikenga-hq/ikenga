// WP-72: the D-05 `locked` overlay. Written, not run (DEC-50).

import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { AppLockStatus } from '@/lib/tauri-cmd';

const mocks = vi.hoisted(() => ({
	appLockStatus: vi.fn(),
	appLockTouch: vi.fn(),
	appLockUnlock: vi.fn(),
	appLockUnlockBiometric: vi.fn(),
	onAppLockChanged: vi.fn(),
}));

vi.mock('@/lib/tauri-cmd', () => mocks);

import { AppLockOverlay } from './app-lock-overlay';
import { resetAppLockSyncForTests, useAppLockStore } from './app-lock-store';

function status(over: Partial<AppLockStatus> = {}): AppLockStatus {
	return {
		locked: true,
		reason: 'idle',
		lockedAtMs: 1,
		idleEnabled: true,
		idleMinutes: 15,
		method: 'pin',
		secretSet: true,
		biometric: { kind: 'none', label: '', available: false, reason: 'no prompt' },
		retryInMs: null,
		attemptsLeft: 3,
		host: 'ned-desktop',
		os: 'Linux 6.8.0',
		configPath: '/home/ned/.local/share/app.ikenga/app-lock.json',
		...over,
	};
}

let root: HTMLDivElement;

beforeEach(() => {
	resetAppLockSyncForTests();
	mocks.onAppLockChanged.mockResolvedValue(() => {});
	mocks.appLockTouch.mockResolvedValue(undefined);
	root = document.createElement('div');
	root.id = 'root';
	document.body.appendChild(root);
});

afterEach(() => {
	cleanup();
	root.remove();
	vi.clearAllMocks();
});

describe('AppLockOverlay', () => {
	it('renders nothing while unlocked', async () => {
		mocks.appLockStatus.mockResolvedValue(status({ locked: false, reason: null }));
		render(<AppLockOverlay />, { container: root });
		await waitFor(() => expect(mocks.appLockStatus).toHaveBeenCalled());
		expect(document.querySelector('[data-state="locked"]')).toBeNull();
		expect(root.hasAttribute('inert')).toBe(false);
	});

	it('covers the app with the data-state="locked" root and makes the app inert', async () => {
		mocks.appLockStatus.mockResolvedValue(status());
		render(<AppLockOverlay />, { container: root });
		const lock = await waitFor(() => {
			const el = document.querySelector('[data-state="locked"]');
			expect(el).not.toBeNull();
			return el as HTMLElement;
		});
		expect(root.contains(lock)).toBe(false);
		expect(root.hasAttribute('inert')).toBe(true);
		expect(screen.getByText('ned-desktop · locked after 15 min idle')).toBeTruthy();
		expect(screen.getByText(/not a security boundary/)).toBeTruthy();
	});

	it('shows the wrong-PIN line and stays locked', async () => {
		mocks.appLockStatus.mockResolvedValue(status());
		mocks.appLockUnlock.mockResolvedValue({
			ok: false,
			error: 'Wrong PIN. Two attempts left before a 30 s wait.',
			status: status({ attemptsLeft: 2 }),
		});
		render(<AppLockOverlay />, { container: root });
		const input = await screen.findByLabelText('PIN or passphrase');
		fireEvent.change(input, { target: { value: '0000' } });
		await act(async () => {
			fireEvent.click(screen.getByRole('button', { name: 'Unlock' }));
		});
		expect(mocks.appLockUnlock).toHaveBeenCalledWith('0000');
		expect(await screen.findByText(/Two attempts left/)).toBeTruthy();
		expect(document.querySelector('[data-state="locked"]')).not.toBeNull();
	});

	it('unlocks with the right PIN and gives the app back', async () => {
		mocks.appLockStatus.mockResolvedValue(status());
		mocks.appLockUnlock.mockResolvedValue({
			ok: true,
			error: null,
			status: status({ locked: false, reason: null }),
		});
		render(<AppLockOverlay />, { container: root });
		const input = await screen.findByLabelText('PIN or passphrase');
		fireEvent.change(input, { target: { value: '1234' } });
		await act(async () => {
			fireEvent.submit(input.closest('form') as HTMLFormElement);
		});
		await waitFor(() => expect(document.querySelector('[data-state="locked"]')).toBeNull());
		expect(root.hasAttribute('inert')).toBe(false);
	});

	it('drops keys aimed outside the lock', async () => {
		mocks.appLockStatus.mockResolvedValue(status());
		render(<AppLockOverlay />, { container: root });
		await screen.findByLabelText('PIN or passphrase');
		const outside = document.createElement('button');
		root.appendChild(outside);
		const seen = vi.fn();
		window.addEventListener('keydown', seen);
		fireEvent.keyDown(outside, { key: 'k', metaKey: true });
		window.removeEventListener('keydown', seen);
		expect(seen).not.toHaveBeenCalled();
	});

	it('draws the OS button disabled, with the reason, where the OS has one', async () => {
		mocks.appLockStatus.mockResolvedValue(
			status({
				biometric: {
					kind: 'windows-hello',
					label: 'Windows Hello',
					available: false,
					reason: "Windows Hello isn't wired into this build yet.",
				},
			})
		);
		render(<AppLockOverlay />, { container: root });
		const btn = await screen.findByRole('button', { name: /Use Windows Hello/ });
		expect((btn as HTMLButtonElement).disabled).toBe(true);
	});

	it('follows the store: an unlock from another window removes it', async () => {
		mocks.appLockStatus.mockResolvedValue(status());
		render(<AppLockOverlay />, { container: root });
		await screen.findByLabelText('PIN or passphrase');
		act(() => useAppLockStore.getState().setStatus(status({ locked: false, reason: null })));
		expect(document.querySelector('[data-state="locked"]')).toBeNull();
	});
});
