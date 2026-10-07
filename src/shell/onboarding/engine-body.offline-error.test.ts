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
});
