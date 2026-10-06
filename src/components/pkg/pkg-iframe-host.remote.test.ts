// Gap audit rank 22 — host.fetch / host.invoke in a remote browser session.
//
// The daemon serves `pkg_is_trusted_for_elevated` (always false there) but
// not `pkg_fetch` / `pkg_invoke`, so a browser pkg used to get "pkg is not
// trusted for elevated capabilities" — wrong advice. It now gets "not
// available in the browser yet" without either command being sent. The
// desktop path still runs the trust check and the call. Raw "not implemented
// in headless daemon" errors from agentOps.runNow are mapped the same way.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({ remote: false }));

vi.mock('@/lib/tauri-cmd', () => ({
	isRemoteWebSession: () => h.remote,
	dbQuery: vi.fn(),
	dbExec: vi.fn(),
	pkgKernelStatus: vi.fn(),
	pkgPreviewManifest: vi.fn(),
	pkgContentHtml: vi.fn(),
	pkgContentRevoke: vi.fn(),
	pkgMcpCall: vi.fn(),
	pkgSidecarCall: vi.fn(),
	pkgIsTrustedForElevated: vi.fn(),
	pkgFetch: vi.fn(),
	pkgInvoke: vi.fn(),
	agentOpsRunNow: vi.fn(),
}));

import {
	agentOpsRunNow,
	pkgFetch,
	pkgInvoke,
	pkgIsTrustedForElevated,
	pkgKernelStatus,
	pkgPreviewManifest,
} from '@/lib/tauri-cmd';
import { dispatchHostCall } from './pkg-iframe-host';

const PKG = 'com.example.remote';

function text(r: { content: Array<{ text?: string }> }): string {
	return r.content.map((c) => c.text ?? '').join('');
}

beforeEach(() => {
	vi.mocked(pkgIsTrustedForElevated).mockReset().mockResolvedValue(true);
	vi.mocked(pkgFetch)
		.mockReset()
		.mockResolvedValue({ ok: true, status: 200 } as never);
	vi.mocked(pkgInvoke)
		.mockReset()
		.mockResolvedValue({ ok: true, exitCode: 0 } as never);
	vi.mocked(agentOpsRunNow).mockReset();
	vi.mocked(pkgKernelStatus).mockResolvedValue({
		installed: [{ id: PKG, install_path: `/pkgs/${PKG}` }],
	} as never);
	vi.mocked(pkgPreviewManifest).mockResolvedValue({
		id: PKG,
		capabilities: {
			http: { allow: ['https://api.example.com'] },
			invoke: { commands: ['ls'] },
			agentOps: true,
		},
	} as never);
});
afterEach(() => {
	h.remote = false;
});

describe('host.fetch / host.invoke in a remote browser session (gap rank 22)', () => {
	it('host.fetch answers "not available in the browser yet" without the trust check or pkg_fetch', async () => {
		h.remote = true;
		const r = await dispatchHostCall(PKG, 'host.fetch', { url: 'https://api.example.com/x' });
		expect(r.isError).toBe(true);
		expect(text(r)).toBe('host.fetch: not available in the browser yet');
		expect(text(r)).not.toMatch(/not trusted/);
		expect(pkgIsTrustedForElevated).not.toHaveBeenCalled();
		expect(pkgFetch).not.toHaveBeenCalled();
	});

	it('host.invoke answers "not available in the browser yet" without the trust check or pkg_invoke', async () => {
		h.remote = true;
		const r = await dispatchHostCall(PKG, 'host.invoke', { command: 'ls' });
		expect(r.isError).toBe(true);
		expect(text(r)).toBe('host.invoke: not available in the browser yet');
		expect(pkgIsTrustedForElevated).not.toHaveBeenCalled();
		expect(pkgInvoke).not.toHaveBeenCalled();
	});

	it('the desktop path still checks trust and calls pkg_fetch', async () => {
		const r = await dispatchHostCall(PKG, 'host.fetch', { url: 'https://api.example.com/x' });
		expect(pkgIsTrustedForElevated).toHaveBeenCalledWith(PKG);
		expect(pkgFetch).toHaveBeenCalledTimes(1);
		expect(r.isError).toBe(false);
	});

	it('the desktop path still refuses an untrusted pkg', async () => {
		vi.mocked(pkgIsTrustedForElevated).mockResolvedValue(false);
		const r = await dispatchHostCall(PKG, 'host.invoke', { command: 'ls' });
		expect(text(r)).toBe('host.invoke: pkg is not trusted for elevated capabilities');
		expect(pkgInvoke).not.toHaveBeenCalled();
	});

	it('maps an unserved agentOps.runNow error to "not available in the browser yet"', async () => {
		vi.mocked(agentOpsRunNow).mockRejectedValue(
			new Error("Command 'agent_ops_run_now' not implemented in headless daemon")
		);
		const r = await dispatchHostCall(PKG, 'host.agentOps.runNow', { jobId: 'j1' });
		expect(text(r)).toBe('host.agentOps.runNow failed: not available in the browser yet');
	});
});
