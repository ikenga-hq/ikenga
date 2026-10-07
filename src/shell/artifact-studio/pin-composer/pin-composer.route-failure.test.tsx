// Gap audit rank 23 — the pin is saved, then routed to a terminal / Chi. A
// route failure used to be only a console.error after the dialog closed; it now
// stays open and says the pin was saved but not sent.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	routePin: vi.fn(),
	commentCreate: vi.fn(async () => ({ id: 7 })),
	pinScreenshotWrite: vi.fn(async () => '/tmp/pin.png'),
}));

vi.mock('@/lib/artifact/route-pin', () => ({
	routePin: h.routePin,
	routeOutcomeLabel: () => 'sent',
}));
vi.mock('@/lib/tauri-cmd', () => ({
	commentCreate: h.commentCreate,
	pinScreenshotWrite: h.pinScreenshotWrite,
}));
vi.mock('@/lib/keymap/dispatcher', () => ({ useCommands: () => {} }));
vi.mock('@/shell/artifact-studio/studio-sink-popover', () => ({
	readArtifactSink: vi.fn(async () => 'inherit'),
	studioSinkToPreferredPtyId: () => null,
	studioSinkToRouteOverride: () => undefined,
}));
vi.mock('@/terminal/session-store', () => ({
	useTerminalStore: { getState: () => ({ tabs: [], activeId: null }) },
}));

import { PinComposer } from './pin-composer';

const PICK = {
	selector: '#a',
	positionX: 0.1,
	positionY: 0.2,
	screenshotBase64: 'AAAA',
	screenshotWidth: 10,
	screenshotHeight: 10,
	elementLabel: 'div',
};

const onClose = vi.fn();

function renderComposer() {
	return render(
		<QueryClientProvider client={new QueryClient()}>
			<PinComposer open pick={PICK} artifactPath="/p/a.html" onClose={onClose} />
		</QueryClientProvider>
	);
}

async function submit() {
	const user = userEvent.setup();
	await user.type(screen.getByPlaceholderText(/What needs to change/), 'move this');
	await user.click(screen.getByRole('button', { name: 'Add pin' }));
}

beforeEach(() => {
	onClose.mockReset();
	h.routePin.mockReset();
});
afterEach(cleanup);

describe('PinComposer route failure (gap rank 23)', () => {
	it('stays open and says the pin was saved but not sent', async () => {
		h.routePin.mockRejectedValue(
			new Error("Command 'comment_route' not implemented in headless daemon")
		);
		renderComposer();
		await submit();
		const alert = await screen.findByRole('alert');
		expect(alert.textContent).toContain('Pin saved');
		expect(alert.textContent).toContain('Not available on this server yet');
		expect(onClose).not.toHaveBeenCalled();
		expect(screen.queryByRole('button', { name: 'Add pin' })).toBeNull();
		// The dialog also has an icon-only "Close" (X); ours is the text button.
		const close = screen
			.getAllByRole('button', { name: 'Close' })
			.find((b) => !b.querySelector('svg'));
		await userEvent.setup().click(close as HTMLElement);
		expect(onClose).toHaveBeenCalledWith(true);
		expect(h.commentCreate).toHaveBeenCalledTimes(1);
	});

	it('closes as committed when routing works', async () => {
		h.routePin.mockResolvedValue({ sink: 'terminal', copied: false });
		renderComposer();
		await submit();
		await waitFor(() => expect(onClose).toHaveBeenCalledWith(true));
		expect(screen.queryByRole('alert')).toBeNull();
	});
});
