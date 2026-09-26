// WP-45 — D-08 `pkg-view` state components. Each state root carries its
// `data-state` (G-55 state map) and offers exactly one next action (WP-43's
// singular `StateAction`). Written, not run (DEC-50: build/test is WP-47's).
//
// No `@testing-library/jest-dom` in this repo — plain DOM assertions only.

import { cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

// The trust sheet pulls react-query + the Ngwa snapshot; neither is under
// test here — only that "Allow host…" mounts it in `violation` mode.
const trustSheetProps = vi.fn();
vi.mock('@/shell/ngwa/ngwa-trust-sheet', () => ({
	NgwaTrustSheet: (props: Record<string, unknown>) => {
		trustSheetProps(props);
		return <div data-testid="trust-sheet" />;
	},
}));
const allowOrigin = vi.fn(async (_pkgId: string, origin: string) => origin);
vi.mock('@/lib/tauri-cmd', () => ({
	pkgWebviewAllowOrigin: (pkgId: string, origin: string) => allowOrigin(pkgId, origin),
}));
vi.mock('@/lib/ngwa/use-ngwa-snapshot', () => ({
	useNgwaSnapshot: () => ({ items: [{ id: 'com.ikenga.studio', name: 'studio' }] }),
}));

import {
	PkgBlockedState,
	PkgBlockedTrustSheet,
	PkgConsentState,
	PkgCrashedState,
	PkgLoadingState,
	PkgSidecarDownStrip,
} from './pkg-view-states';

// Sheets portal to document.body; unmount between tests so one test's sheet
// can't satisfy (or break) the next test's body-wide query.
afterEach(cleanup);

const PKG = 'com.ikenga.studio';
const BLOCKED = { host: 'fal.media', target: 'https://fal.media/files/x', scope: 'csp:frame-src' };
const WEBVIEW_BLOCKED = {
	host: 'fal.media',
	target: 'https://fal.media/files/x',
	scope: 'webview:allowed_origins',
	origin: 'https://fal.media',
};

function root(container: HTMLElement, state: string) {
	const el = container.querySelector(`[data-state="${state}"]`);
	expect(el).not.toBeNull();
	return el as HTMLElement;
}

describe('pkg-loading', () => {
	it('ember pulse + handshake steps, no spinner, no action', () => {
		const { container } = render(<PkgLoadingState pkgId={PKG} source="dist/index.html" phase="handshake" />);
		const el = root(container, 'pkg-loading');
		expect(el.querySelector('.ember-dots')).not.toBeNull();
		expect(el.querySelector('.animate-spin')).toBeNull();
		expect(el.querySelectorAll('button')).toHaveLength(0);
		const now = el.querySelector('[data-step-status="now"]');
		expect(now?.getAttribute('data-step')).toBe('handshake');
		expect(el.textContent).toContain('Loading package…');
	});

	it('overlay mode wraps the state over the frame', () => {
		const { container } = render(
			<PkgLoadingState pkgId={PKG} source="dist/index.html" phase="handshake" overlay />
		);
		const el = root(container, 'pkg-loading');
		expect(el.parentElement?.className).toContain('absolute');
	});
});

describe('pkg-consent', () => {
	it('inline prompt with the added capabilities and one Allow action', () => {
		const onAllow = vi.fn();
		const { container } = render(
			<PkgConsentState
				pkgId={PKG}
				review={{
					pkg_id: PKG,
					manifest_version: '0.3.0',
					old_capabilities: '{"permissions":{}}',
					new_capabilities: '{"permissions":{"fs":["read"]}}',
					prior_approved_at_ms: 0,
				}}
				onAllow={onAllow}
			/>
		);
		const el = root(container, 'pkg-consent');
		expect(el.getAttribute('role')).not.toBe('dialog');
		expect(el.textContent).toContain('permissions.fs = read');
		const buttons = el.querySelectorAll('button');
		expect(buttons).toHaveLength(1);
		expect(buttons[0].textContent).toBe('Allow');
		fireEvent.click(buttons[0]);
		expect(onAllow).toHaveBeenCalledTimes(1);
	});
});

describe('pkg-crashed', () => {
	it('shows the shipped copy + error and one Reload view action', () => {
		const onReload = vi.fn();
		const { container } = render(
			<PkgCrashedState
				pkgId={PKG}
				source="dist/index.html"
				error="bridge connect failed: timeout"
				onReload={onReload}
			/>
		);
		const el = root(container, 'pkg-crashed');
		expect(el.textContent).toContain('Failed to load package UI');
		expect(el.textContent).toContain('bridge connect failed: timeout');
		const buttons = el.querySelectorAll('button');
		expect(buttons).toHaveLength(1);
		expect(buttons[0].textContent).toBe('Reload view');
		fireEvent.click(buttons[0]);
		expect(onReload).toHaveBeenCalledTimes(1);
	});
});

describe('pkg-sidecar-down', () => {
	it('strip with the reason and one Restart action', () => {
		const onRestart = vi.fn();
		const { container } = render(
			<PkgSidecarDownStrip reason="3 strikes in 60 s" onRestart={onRestart} />
		);
		const el = root(container, 'pkg-sidecar-down');
		expect(el.textContent).toContain('3 strikes in 60 s');
		const buttons = el.querySelectorAll('button');
		expect(buttons).toHaveLength(1);
		expect(buttons[0].textContent).toBe('Restart');
		fireEvent.click(buttons[0]);
		expect(onRestart).toHaveBeenCalledTimes(1);
	});
});

describe('pkg-blocked', () => {
	it('webview block: names the host, cites allowed_origins, one Allow host… action', () => {
		const onAllowHost = vi.fn();
		const { container } = render(
			<PkgBlockedState pkgId={PKG} blocked={WEBVIEW_BLOCKED} onAllowHost={onAllowHost} />
		);
		const el = root(container, 'pkg-blocked');
		expect(el.textContent).toContain('Blocked a navigation to fal.media');
		expect(el.textContent).toContain('capabilities.webview.allowed_origins');
		const buttons = el.querySelectorAll('button');
		expect(buttons).toHaveLength(1);
		expect(buttons[0].textContent).toBe('Allow host…');
		fireEvent.click(buttons[0]);
		expect(onAllowHost).toHaveBeenCalledTimes(1);
	});

	it('iframe CSP block: no Allow host (no grant can lift it), Reload view instead', () => {
		const onAllowHost = vi.fn();
		const onReload = vi.fn();
		const { container } = render(
			<PkgBlockedState
				pkgId={PKG}
				blocked={BLOCKED}
				onAllowHost={onAllowHost}
				onReload={onReload}
			/>
		);
		const el = root(container, 'pkg-blocked');
		expect(el.textContent).toContain('Allow host is not available');
		const buttons = el.querySelectorAll('button');
		expect(buttons).toHaveLength(1);
		expect(buttons[0].textContent).toBe('Reload view');
		fireEvent.click(buttons[0]);
		expect(onReload).toHaveBeenCalledTimes(1);
		expect(onAllowHost).not.toHaveBeenCalled();
	});

	it('an inline script block is not called a navigation', () => {
		const { container } = render(
			<PkgBlockedState
				pkgId={PKG}
				blocked={{ host: 'inline', target: 'inline', scope: 'csp:script-src' }}
				onAllowHost={() => {}}
			/>
		);
		const text = root(container, 'pkg-blocked').textContent ?? '';
		expect(text).toContain('Blocked an inline script');
		expect(text).not.toContain('navigation to inline');
	});

	it('Allow host… opens a scoped violation sheet that grants only this origin', async () => {
		trustSheetProps.mockClear();
		allowOrigin.mockClear();
		const { queryByTestId, rerender } = render(
			<PkgBlockedTrustSheet
				pkgId={PKG}
				blocked={WEBVIEW_BLOCKED}
				open={false}
				onOpenChange={() => {}}
			/>
		);
		expect(queryByTestId('trust-sheet')).toBeNull();
		rerender(
			<PkgBlockedTrustSheet pkgId={PKG} blocked={WEBVIEW_BLOCKED} open onOpenChange={() => {}} />
		);
		// Portaled to document.body, out of any pooled-surface stacking context.
		expect(document.body.querySelector('[data-testid="trust-sheet"]')).not.toBeNull();
		const props = trustSheetProps.mock.calls.at(-1)?.[0] as Record<string, unknown>;
		expect(props.mode).toBe('violation');
		expect(props.violationTarget).toBe('https://fal.media');
		expect(props.violationScopeKind).toBe('webview:allowed_origins');
		expect((props.item as { id: string }).id).toBe(PKG);
		const grant = props.violationGrant as { run: () => Promise<void> };
		await grant.run();
		expect(allowOrigin).toHaveBeenCalledWith(PKG, 'https://fal.media');
	});

	it('never mounts the sheet for a CSP block', () => {
		const { queryByTestId } = render(
			<PkgBlockedTrustSheet pkgId={PKG} blocked={BLOCKED} open onOpenChange={() => {}} />
		);
		expect(queryByTestId('trust-sheet')).toBeNull();
	});
});
