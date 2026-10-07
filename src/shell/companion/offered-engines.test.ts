// D-10/D-11: an engine detection couldn't check (WSL couldn't be asked) stays
// offered when it is the default (tagged), is otherwise not offered, and is
// what the empty state names when nothing else is.

import { describe, expect, it } from 'vitest';

import type { DetectedAgent } from '@/lib/tauri-cmd';

import { engineOfferNote, offeredEngines, unavailableEngine } from './offered-engines';

function agent(id: string, over: Partial<DetectedAgent> = {}): DetectedAgent {
	return {
		id,
		display: id,
		executable_path: `/usr/bin/${id}`,
		version: '1.0.0',
		authed: true,
		auth_hint: null,
		capabilities: {
			streaming: true,
			tool_use: true,
			thinking: false,
			artifacts: false,
			mcp: false,
			session_resume: false,
		},
		...over,
	};
}

const wslDown = { kind: 'wsl', reason: 'no distro' } as const;

describe('offeredEngines', () => {
	it('offers the default first, then every other runnable engine', () => {
		expect(offeredEngines('codex', [agent('claude-code'), agent('codex')])).toEqual([
			'codex',
			'claude-code',
		]);
	});

	it('skips signed-out engines', () => {
		expect(offeredEngines(null, [agent('claude-code', { authed: false }), agent('codex')])).toEqual(
			['codex']
		);
	});

	it('keeps an unchecked default engine seatable (D-11), drops other unchecked ones', () => {
		const detected = [
			agent('claude-code', { authed: null, version: null, unavailable: wslDown }),
			agent('gemini', { authed: null, version: null, unavailable: wslDown }),
			agent('codex'),
		];
		expect(offeredEngines('claude-code', detected)).toEqual(['claude-code', 'codex']);
		expect(offeredEngines('codex', detected)).toEqual(['codex']);
		expect(offeredEngines(null, detected)).toEqual(['codex']);
	});

	it('still offers a default engine that is absent from detection (unchanged)', () => {
		expect(offeredEngines('claude-code', [])).toEqual(['claude-code']);
	});
});

describe('engineOfferNote', () => {
	it('tags only an engine detection could not check', () => {
		const detected = [agent('claude-code', { unavailable: wslDown }), agent('codex')];
		expect(engineOfferNote('claude-code', detected)).toBe('WSL unavailable');
		expect(engineOfferNote('codex', detected)).toBeNull();
		expect(engineOfferNote('gemini', undefined)).toBeNull();
	});
});

describe('unavailableEngine', () => {
	it('is null when every engine was checked', () => {
		expect(unavailableEngine('codex', [agent('codex')])).toBeNull();
		expect(unavailableEngine(null, undefined)).toBeNull();
	});

	it('prefers the default engine, else the first unavailable one', () => {
		const detected = [
			agent('codex', { unavailable: wslDown }),
			agent('claude-code', { unavailable: wslDown }),
		];
		expect(unavailableEngine('claude-code', detected)?.id).toBe('claude-code');
		expect(unavailableEngine(null, detected)?.id).toBe('codex');
	});
});
