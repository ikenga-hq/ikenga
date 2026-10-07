import { describe, expect, it } from 'vitest';
import {
	fullyDown,
	isPartiallyUnreadable,
	notScannedReason,
	rootNotScanned,
} from './scan-coverage';

// The exact shape `server::shared::ngwa::partial_scan_error` writes (pinned by
// the Rust test `config_errors_read_partially_unreadable`).
const BOTH =
	'partially unreadable — project roots not scanned: `/x/far` (path outside allowlist: /x/far); ' +
	'`/y/gone/` (expand: no such dir) · unreadable: `/x/near/.claude/settings.json` (hooks parse: eof)';

describe('scan coverage (a partially unreadable config scan)', () => {
	it('recognises a partial scan, and keeps only outright failures as "down"', () => {
		expect(isPartiallyUnreadable(BOTH)).toBe(true);
		expect(isPartiallyUnreadable('HOME unset')).toBe(false);
		expect(isPartiallyUnreadable(null)).toBe(false);
		const down = fullyDown([
			{ source: 'engine_config', error: BOTH },
			{ source: 'oba', error: '--data-dir not set' },
		]);
		expect(down.map((s) => s.source)).toEqual(['oba']);
	});

	it('names each refused project root with its message, and nothing else', () => {
		expect(rootNotScanned(BOTH, '/x/far')).toBe('path outside allowlist: /x/far');
		expect(rootNotScanned(BOTH, '/y/gone/')).toBe('expand: no such dir');
		expect(rootNotScanned(BOTH, '/y/gone')).toBe('expand: no such dir');
		// A readable project whose FILE failed is not "not scanned".
		expect(rootNotScanned(BOTH, '/x/near')).toBeNull();
		expect(rootNotScanned(BOTH, '/x/near/.claude/settings.json')).toBeNull();
		expect(rootNotScanned(BOTH, null)).toBeNull();
		// A trailing slash on the column's root still matches.
		expect(rootNotScanned(BOTH, '/x/far/')).toBe('path outside allowlist: /x/far');
		// Not a partial-scan error: nothing is "not scanned".
		expect(rootNotScanned('path outside allowlist: `/x/far` (x)', '/x/far')).toBeNull();
	});

	it('the last entry, with no file errors after it, keeps its whole message', () => {
		const one =
			'partially unreadable — project roots not scanned: `/x/far` (path outside allowlist: /x/far)';
		expect(rootNotScanned(one, '/x/far')).toBe('path outside allowlist: /x/far');
	});

	it('words the column reason for the allowlist case and the generic case', () => {
		expect(notScannedReason('far', 'path outside allowlist: /x/far')).toMatch(
			/outside this server's allowlist/
		);
		expect(notScannedReason('gone', 'expand: no such dir')).toMatch(/could not be read \(expand/);
	});
});
