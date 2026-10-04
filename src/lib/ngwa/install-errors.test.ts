import { describe, expect, it } from 'vitest';
import {
	classifyInstallError,
	classifyInstallKind,
	dedupeInstallLog,
	extractDebugLogPath,
} from './install-errors';

const ENOSPC_LINE =
	"npm warn tar TAR_ENTRY_ERROR ENOSPC: no space left on device, write";

/** The shape of the real failure: the Rust chain with npm's stderr inside,
 *  one warning per file npm could not write. */
function enospcFailure(files = 40): string {
	const warns = Array.from(
		{ length: files },
		(_, i) => `${ENOSPC_LINE} 'C:\\Users\\x\\pkgs\\com.ikenga.meetings\\node_modules\\a\\f${i}.js'`
	);
	return [
		'npm dependency materialization failed: ' + warns[0],
		...warns.slice(1),
		'npm error code ENOSPC',
		'npm error A complete log of this run can be found in: C:\\Users\\x\\AppData\\Local\\npm-cache\\_logs\\2026-10-03T13_51_02_123Z-debug-0.log',
	].join('\n');
}

describe('classifyInstallKind', () => {
	it.each([
		['disk-space', enospcFailure()],
		['disk-space', 'write tarball chunk: There is not enough space on the disk. (os error 112)'],
		['network', 'npm error code EAI_AGAIN\nnpm error request to https://registry.npmjs.org/ajv failed'],
		['network', 'npm error code ECONNRESET'],
		['network', 'npm error code ETIMEDOUT'],
		['network', 'download https://x: GET https://x: error sending request for url'],
		['not-found', 'download https://x: HTTP error from https://x: HTTP status client error (404 Not Found) for url (https://x)'],
		['not-found', 'npm error code E404\nnpm error 404 Not Found - GET https://registry.npmjs.org/nope'],
		['permission', 'npm error code EPERM\nnpm error syscall rename'],
		['permission', 'npm error code EACCES'],
		['permission', 'backup existing install: Access is denied. (os error 5)'],
		['integrity', 'tarball SHA-512 integrity mismatch — refusing to install'],
		['integrity', 'npm error code EINTEGRITY'],
		['cancelled', 'install cancelled'],
		['unknown', 'manifest id mismatch: tarball declares `a`, registry said `b`'],
	] as const)('%s ← %s', (kind, raw) => {
		expect(classifyInstallKind(raw)).toBe(kind);
	});
});

describe('classifyInstallError', () => {
	it('gives the disk-space message with the pkg name', () => {
		const c = classifyInstallError(new Error(enospcFailure()), 'Meetings');
		expect(c.kind).toBe('disk-space');
		expect(c.message).toBe(
			'Not enough disk space to install Meetings. Free some space and try again.'
		);
		expect(c.retryable).toBe(true);
	});

	it('pulls out the npm debug log path', () => {
		expect(extractDebugLogPath(enospcFailure())).toBe(
			'C:\\Users\\x\\AppData\\Local\\npm-cache\\_logs\\2026-10-03T13_51_02_123Z-debug-0.log'
		);
		expect(extractDebugLogPath('no log here')).toBeNull();
	});

	it('names an unknown failure without echoing the raw text', () => {
		const c = classifyInstallError('something odd', 'Notes');
		expect(c.kind).toBe('unknown');
		expect(c.message).toContain('Notes');
		expect(c.message).not.toContain('something odd');
		expect(c.details).toBe('something odd');
	});
});

describe('dedupeInstallLog', () => {
	it('collapses the repeated ENOSPC warnings to one line with a count', () => {
		const out = dedupeInstallLog(enospcFailure(40));
		const lines = out.split('\n');
		expect(lines.filter((l) => l.includes('TAR_ENTRY_ERROR')).length).toBe(2);
		expect(out).toContain('(×39)');
		expect(out).toContain('npm error code ENOSPC');
	});

	it('truncates long logs and says so', () => {
		const raw = Array.from({ length: 100 }, (_, i) => `distinct line number ${i} ${'x'.repeat(i)}`).join(
			'\n'
		);
		const out = dedupeInstallLog(raw);
		expect(out.split('\n').length).toBeLessThanOrEqual(41);
		expect(out).toMatch(/truncated \(60 more lines\)$/);
	});

	it('drops blank lines', () => {
		expect(dedupeInstallLog('a\n\n\nb\n')).toBe('a\nb');
	});
});
