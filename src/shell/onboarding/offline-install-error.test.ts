import { describe, expect, it } from 'vitest';
import { offlineInstallErrorMessage } from './offline-install-error';

describe('offlineInstallErrorMessage', () => {
	it('names the registry only for a network failure', () => {
		expect(offlineInstallErrorMessage(new TypeError('Failed to fetch'))).toMatch(
			/Couldn't reach the package registry/
		);
		expect(offlineInstallErrorMessage(new Error('npm error code EAI_AGAIN'))).toMatch(
			/Couldn't reach the package registry/
		);
	});

	it('does not blame the registry for a disk, permission or integrity failure', () => {
		for (const raw of [
			'write tarball chunk: There is not enough space on the disk. (os error 112)',
			'backup existing install: Access is denied. (os error 5)',
			'tarball SHA-512 integrity mismatch — refusing to install',
			'manifest id mismatch: tarball declares `a`, registry said `b`',
		]) {
			const msg = offlineInstallErrorMessage(new Error(raw));
			expect(msg).not.toMatch(/reach/i);
			expect(msg).toMatch(/Ngwa → Store/);
		}
	});

	it('reports a failed index signature check as a verification failure', () => {
		expect(offlineInstallErrorMessage(new Error('index signature verification failed'))).toMatch(
			/couldn't be verified/
		);
	});
});
