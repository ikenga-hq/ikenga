// `/iyke/mode` allow-list (G-STATE, g-state.md §5): the four v16 modes pass,
// pre-v16 names are normalized with a warning for one release, anything else
// is rejected.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ACTIVITY_MODES } from '@/lib/shell/shell-store';

import { resolveIykeMode } from './control-listener';

// The listener module imports the terminal stack (xterm needs a canvas jsdom
// lacks); the allow-list under test doesn't touch it.
vi.mock('@/terminal/single-terminal', () => ({ createTerminalSession: vi.fn() }));

describe('resolveIykeMode', () => {
	let warn: ReturnType<typeof vi.spyOn>;
	beforeEach(() => {
		warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
	});
	afterEach(() => {
		vi.restoreAllMocks();
	});

	it('accepts the four v16 modes as-is, without a warning', () => {
		expect(ACTIVITY_MODES).toEqual(['project', 'chi', 'ngwa', 'settings']);
		for (const m of ACTIVITY_MODES) {
			expect(resolveIykeMode(m)).toBe(m);
		}
		expect(warn).not.toHaveBeenCalled();
	});

	it('normalizes legacy mode names with a warning', () => {
		expect(resolveIykeMode('app')).toBe('project');
		expect(resolveIykeMode('files')).toBe('project');
		expect(resolveIykeMode('sessions')).toBe('project');
		expect(resolveIykeMode('artifact-grid')).toBe('project');
		expect(resolveIykeMode('pkg:com.ikenga.tasks')).toBe('project');
		expect(resolveIykeMode('pkgs')).toBe('ngwa');
		expect(warn).toHaveBeenCalledTimes(6);
		expect(String(warn.mock.calls[0]![0])).toContain('legacy mode name');
	});

	it('rejects garbage', () => {
		for (const m of [
			'mail',
			'studio',
			'PROJECT',
			'',
			' project',
			42,
			null,
			undefined,
			{},
			['chi'],
		]) {
			expect(resolveIykeMode(m)).toBeNull();
		}
		expect(warn).toHaveBeenCalled();
	});
});
