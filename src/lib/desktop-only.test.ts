import { afterEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({ remote: false }));
vi.mock('@/lib/tauri-cmd', () => ({ isRemoteWebSession: () => h.remote }));

import {
	installSourceBlock,
	installUnavailableReason,
	isLocalSource,
	LOCAL_INSTALL_DESKTOP_ONLY_REASON,
	NOT_AVAILABLE_ON_SERVER_YET,
	PACKAGES_OPERATOR_REASON,
	pkgInstallUnavailableReason,
	REMOTE_SOURCE_HTTPS_REASON,
	remoteSourceBlock,
} from './desktop-only';
import { honestRpcError } from './transport/unavailable';

afterEach(() => {
	h.remote = false;
});

describe('installUnavailableReason (gap rank 3)', () => {
	it('is the honest sentence in a remote session', () => {
		h.remote = true;
		expect(installUnavailableReason()).toBe('Not available on this server yet');
		expect(NOT_AVAILABLE_ON_SERVER_YET).toBe('Not available on this server yet');
	});
	it('is false on the desktop', () => {
		expect(installUnavailableReason()).toBe(false);
	});
});

describe('honestRpcError', () => {
	it('keeps a genuine failure that merely contains a loose phrase', () => {
		for (const m of [
			'npm ERR! engine node@16 is not supported by this package',
			'pkg manifest: unknown command "foo" in run block',
		])
			expect(honestRpcError(new Error(m))).toBe(m);
	});
	it('never lets a raw "not implemented" string through', () => {
		expect(
			honestRpcError(
				new Error("Command 'pkg_install_from_path' not implemented in headless daemon")
			)
		).toBe('Not available on this server yet');
	});
	it('keeps a real failure as itself', () => {
		expect(honestRpcError(new Error('EACCES: permission denied'))).toBe(
			'EACCES: permission denied'
		);
		expect(honestRpcError('boom')).toBe('boom');
	});
});

describe('pkgInstallUnavailableReason (Ngwa Store: packages)', () => {
	it('says the server operator installs packages, in a remote session', () => {
		h.remote = true;
		expect(pkgInstallUnavailableReason()).toBe('Packages are installed by the server operator');
		expect(PACKAGES_OPERATOR_REASON).toBe('Packages are installed by the server operator');
	});
	it('is false on the desktop', () => {
		expect(pkgInstallUnavailableReason()).toBe(false);
	});
});

describe('remoteSourceBlock (Ngwa Store: Add from URL)', () => {
	const local = [
		'/home/me/skills/pdf',
		'~/skills/pdf',
		'./pdf',
		'../pdf',
		'..',
		'C:\\Users\\me\\pdf',
		'c:/Users/me/pdf',
		'file:///home/me/pdf',
		'npx skills add /home/me/pdf',
	];
	it.each(local)('names %s a local install in a remote session', (src) => {
		expect(isLocalSource(src)).toBe(true);
		expect(remoteSourceBlock(src, true)).toBe('Local installs are desktop-only');
		expect(LOCAL_INSTALL_DESKTOP_ONLY_REASON).toBe('Local installs are desktop-only');
	});
	it.each([
		'git@github.com:o/r.git',
		'ssh://git@github.com/o/r',
		'git://h/o/r',
		'http://h.com/o/r',
		'ext::sh -c x',
		'--upload-pack=x',
	])('names %s a non-https source in a remote session', (src) => {
		expect(remoteSourceBlock(src, true)).toBe(REMOTE_SOURCE_HTTPS_REASON);
	});
	it.each([
		'https://github.com/o/r',
		'https://github.com/o/r.git',
		'o/r',
		'github:o/r',
		'npx skills add o/r',
		'',
		'   ',
	])('takes %j', (src) => {
		expect(remoteSourceBlock(src, true)).toBeNull();
	});
	it('never blocks on the desktop, whatever the source', () => {
		for (const src of [...local, 'git@github.com:o/r.git', 'file:///x'])
			expect(remoteSourceBlock(src, false)).toBeNull();
	});
	it('installSourceBlock follows the session', () => {
		h.remote = true;
		expect(installSourceBlock('/x/y')).toBe('Local installs are desktop-only');
		h.remote = false;
		expect(installSourceBlock('/x/y')).toBeNull();
	});
});
