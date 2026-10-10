import { describe, expect, it } from 'vitest';
import { LOCAL_ECHO_STORAGE_KEY, parseLocalEchoMode, useLocalEchoSettings } from './settings';

describe('local echo settings', () => {
	it('defaults to auto and rejects unknown values', () => {
		expect(parseLocalEchoMode(null)).toBe('auto');
		expect(parseLocalEchoMode('sometimes')).toBe('auto');
		expect(parseLocalEchoMode('always')).toBe('always');
		expect(parseLocalEchoMode('off')).toBe('off');
	});

	it('persists the choice per browser', () => {
		useLocalEchoSettings.getState().setMode('off');
		expect(useLocalEchoSettings.getState().mode).toBe('off');
		expect(localStorage.getItem(LOCAL_ECHO_STORAGE_KEY)).toBe('off');
		useLocalEchoSettings.getState().setMode('auto');
		expect(localStorage.getItem(LOCAL_ECHO_STORAGE_KEY)).toBe('auto');
	});
});
