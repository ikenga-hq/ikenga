// Gap audit ranks 20 and 8 (UI halves): in a browser session the webview
// "Clear session" control and the local-viewer-server menu rows are not
// offered; on the desktop they are, and a failed clear says so.

import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	remote: false,
	clearSession: vi.fn(async () => {}),
	webview: { pkg_id: 'com.x.browser', kind: 'webview' } as unknown,
}));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	isRemoteWebSession: () => h.remote,
	pkgWebviewClearSession: h.clearSession,
}));
vi.mock('./pane-views', () => ({ useWebviewRoute: () => h.webview }));

import type { ResolvedMenuRow } from '@/shell/menu/resolve';
import { WebviewSessionControl } from './pane-toolbar';
import { dropRemoteHiddenRows } from './remote-menu-rows';

const VIEW = { kind: 'route', path: '/pkg/com.x.browser/' } as never;

beforeEach(() => {
	h.clearSession.mockReset();
	h.clearSession.mockResolvedValue(undefined);
});
afterEach(() => {
	cleanup();
	h.remote = false;
});

describe('WebviewSessionControl (gap rank 20)', () => {
	it('renders nothing in a remote session', () => {
		h.remote = true;
		const { container } = render(<WebviewSessionControl view={VIEW} paneId="p1" />);
		expect(container.firstChild).toBeNull();
	});

	it('renders on the desktop, and a failed clear is shown rather than dropped', async () => {
		h.clearSession.mockRejectedValue(new Error('jar locked'));
		const user = userEvent.setup();
		render(<WebviewSessionControl view={VIEW} paneId="p1" />);
		await user.click(screen.getByRole('button', { name: /Session:/ }));
		await user.click(await screen.findByRole('button', { name: /Clear session now/ }));
		await waitFor(() => expect(screen.getByRole('alert').textContent).toMatch(/jar locked/));
		expect(h.clearSession).toHaveBeenCalledWith('com.x.browser', 'p1');
	});
});

describe('dropRemoteHiddenRows (gap rank 8)', () => {
	const item = (id: string) => ({ kind: 'item', id }) as unknown as ResolvedMenuRow;
	const sep = { kind: 'separator' } as ResolvedMenuRow;
	const rows = [
		item('pane.back'),
		sep,
		item('viewer.open-in-browser'),
		item('viewer.copy-url'),
		sep,
		item('pane.close'),
	];

	it('retains Open in browser / Copy viewer URL in remote sessions', () => {
		h.remote = true;
		const ids = dropRemoteHiddenRows(rows).map((r) => (r.kind === 'item' ? r.id : '—'));
		expect(ids).toEqual([
			'pane.back',
			'—',
			'viewer.open-in-browser',
			'viewer.copy-url',
			'—',
			'pane.close',
		]);
	});

	it('leaves the desktop menu untouched', () => {
		expect(dropRemoteHiddenRows(rows)).toBe(rows);
	});
});
