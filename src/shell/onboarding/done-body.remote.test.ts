// Gap audit rank 3 — in a browser session an Ngwa step that recorded no install
// outcome says packages can't be installed here, not "status unknown".

import { describe, expect, it, vi } from 'vitest';

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	isRemoteWebSession: () => true,
}));

import { describeInstalls } from './done-body';

describe('describeInstalls in a browser session', () => {
	it('names the real reason when nothing was recorded', () => {
		const r = describeInstalls({
			selected: ['a', 'b'],
			connectorsConfigured: [],
			connectorsSkipped: [],
		});
		expect(r.headline).toBe('2 packages selected, none installed');
		expect(r.problems[0]).toContain('Not available on this server yet');
		expect(r.problems[0]).not.toMatch(/unknown/i);
	});
});
