// useAgentDetect — state-transition tests against the exposed reducer
// helpers. The full hook lives behind `useState`/`useEffect` plumbing
// that `@testing-library/react` would normally exercise, but it isn't a
// dep of this project (see use-pane-history.test.ts for the same
// pattern). Covering the run-token guard + entry mapping directly gives
// us the same confidence without the dep.

import { describe, expect, it } from 'vitest';

import type { DetectedAgent } from '@/lib/tauri-cmd';

import { applyProbeResult, computeAllMissing, entryFromProbe, pendingMap } from './use-agent-detect';

function makeAgent(id: string): DetectedAgent {
	return {
		id,
		display: id,
		executable_path: `/usr/local/bin/${id}`,
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
	};
}

describe('pendingMap', () => {
	it('seeds every requested id as pending', () => {
		const map = pendingMap(['claude-code', 'codex']);
		expect(map['claude-code'].status).toBe('pending');
		expect(map.codex.status).toBe('pending');
	});
});

describe('entryFromProbe', () => {
	it('returns detected when an agent is supplied', () => {
		const entry = entryFromProbe(makeAgent('claude-code'));
		expect(entry.status).toBe('detected');
		expect(entry.agent?.executable_path).toBe('/usr/local/bin/claude-code');
	});

	it('returns missing when the probe resolves to null', () => {
		const entry = entryFromProbe(null);
		expect(entry.status).toBe('missing');
		expect(entry.error).toBeUndefined();
	});

	it('returns unavailable (not missing, not detected) when WSL could not be asked', () => {
		const agent = {
			...makeAgent('claude-code'),
			executable_path: 'claude (WSL)',
			version: null,
			authed: null,
			unavailable: { kind: 'wsl', reason: 'Wsl/Service/CreateInstance/E_FAIL' },
		};
		const entry = entryFromProbe(agent);
		expect(entry.status).toBe('unavailable');
		expect(entry.agent?.id).toBe('claude-code');
		expect(entry.error).toBe('WSL unavailable — Wsl/Service/CreateInstance/E_FAIL');
	});

	it('treats an explicit null unavailable (older/other daemons) as detected', () => {
		const entry = entryFromProbe({ ...makeAgent('codex'), unavailable: null });
		expect(entry.status).toBe('detected');
	});

	it('returns unknown with the error message when the probe rejects', () => {
		const entry = entryFromProbe(null, new Error('boom'));
		expect(entry.status).toBe('unknown');
		expect(entry.error).toContain('boom');
	});
});

describe('applyProbeResult (run-token guard)', () => {
	const seed = pendingMap(['claude-code', 'codex']);

	it('flips a single entry when the token matches the current run', () => {
		const next = applyProbeResult(
			seed,
			'claude-code',
			entryFromProbe(makeAgent('claude-code')),
			1,
			1
		);
		expect(next['claude-code'].status).toBe('detected');
		expect(next.codex.status).toBe('pending');
	});

	it('drops resolutions from a superseded run (pending → detected → pending)', () => {
		const afterRefresh = pendingMap(['claude-code']);
		const stale = applyProbeResult(
			afterRefresh,
			'claude-code',
			entryFromProbe(makeAgent('claude-code')),
			1, // stale token — first run
			2 // current token — second run
		);
		expect(stale['claude-code'].status).toBe('pending');
	});

	it('idempotently flips a card from pending → detected → missing across two runs', () => {
		let map = pendingMap(['claude-code']);
		map = applyProbeResult(map, 'claude-code', entryFromProbe(makeAgent('claude-code')), 1, 1);
		expect(map['claude-code'].status).toBe('detected');

		// refresh() resets the map and bumps the token.
		map = pendingMap(['claude-code']);
		expect(map['claude-code'].status).toBe('pending');

		map = applyProbeResult(map, 'claude-code', entryFromProbe(null), 2, 2);
		expect(map['claude-code'].status).toBe('missing');
	});

	it('mapping the same id twice in one run keeps the latest value', () => {
		let map = pendingMap(['claude-code']);
		map = applyProbeResult(map, 'claude-code', entryFromProbe(null), 1, 1);
		expect(map['claude-code'].status).toBe('missing');
		map = applyProbeResult(map, 'claude-code', entryFromProbe(makeAgent('claude-code')), 1, 1);
		expect(map['claude-code'].status).toBe('detected');
	});
});

describe('computeAllMissing', () => {
	const ids = ['claude-code', 'codex', 'gemini'];

	it('returns false when at least one engine is detected', () => {
		const results = {
			'claude-code': { status: 'detected' as const },
			codex: { status: 'missing' as const },
			gemini: { status: 'missing' as const },
		};
		expect(computeAllMissing(results, ids)).toBe(false);
	});

	it('returns false when any engine is pending', () => {
		const results = {
			'claude-code': { status: 'pending' as const },
			codex: { status: 'missing' as const },
			gemini: { status: 'missing' as const },
		};
		expect(computeAllMissing(results, ids)).toBe(false);
	});

	it('returns false when an engine could not be checked (WSL unavailable)', () => {
		const results = {
			'claude-code': { status: 'unavailable' as const, error: 'WSL unavailable — no distro' },
			codex: { status: 'missing' as const },
			gemini: { status: 'missing' as const },
		};
		expect(computeAllMissing(results, ids)).toBe(false);
	});

	it('returns false when any engine is unknown (probe failed/rejected)', () => {
		const results = {
			'claude-code': { status: 'unknown' as const, error: 'Command not implemented' },
			codex: { status: 'missing' as const },
			gemini: { status: 'missing' as const },
		};
		expect(computeAllMissing(results, ids)).toBe(false);
	});

	it('returns true when all engines are missing', () => {
		const results = {
			'claude-code': { status: 'missing' as const },
			codex: { status: 'missing' as const },
			gemini: { status: 'missing' as const },
		};
		expect(computeAllMissing(results, ids)).toBe(true);
	});
});
