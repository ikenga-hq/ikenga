// Gap audit rank 3 — the offline-engine install used to blame "the registry"
// for every failure. It now says the real cause.
import { describe, expect, it } from 'vitest';

import { offlineInstallErrorMessage } from './offline-install-error';

describe('offlineInstallErrorMessage', () => {
	it('does not blame the registry when the daemon serves no install', () => {
		const msg = offlineInstallErrorMessage(
			new Error("Command 'pkg_install_from_registry' not implemented in headless daemon")
		);
		expect(msg).toContain('Not available on this server yet');
		expect(msg).not.toMatch(/reach the registry/i);
		expect(msg).not.toMatch(/not implemented/i);
	});

	it('carries the real failure text through', () => {
		const msg = offlineInstallErrorMessage(new Error('tarball integrity mismatch'));
		expect(msg).toContain('tarball integrity mismatch');
		expect(msg).not.toMatch(/reach the registry/i);
	});

	it('names a failed signature check as a verification failure, not a network one', () => {
		const msg = offlineInstallErrorMessage(
			new Error('registry index signature verification failed')
		);
		expect(msg).toMatch(
			/couldn't be verified \(signature check failed\), so nothing was installed/
		);
		expect(msg).not.toMatch(/reach/i);
	});

	it('reads a bare webview fetch failure as a network error', () => {
		const msg = offlineInstallErrorMessage(new TypeError('Failed to fetch'));
		expect(msg).toContain("the registry couldn't be reached (network error)");
		expect(msg).not.toContain('Failed to fetch');
	});

	it('keeps an integrity mismatch as its own failure text', () => {
		const msg = offlineInstallErrorMessage(new Error('tarball integrity mismatch (signature ok)'));
		expect(msg).toContain('tarball integrity mismatch');
	});
});
