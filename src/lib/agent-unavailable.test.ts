import { describe, expect, it } from 'vitest';

import type { DetectedAgent } from '@/lib/tauri-cmd';

import { agentUnavailableText, engineFacts } from './agent-unavailable';

function live(over: Partial<DetectedAgent> = {}): DetectedAgent {
	return {
		id: 'claude-code',
		display: 'Claude Code',
		executable_path: '/usr/bin/claude',
		version: '2.0.0',
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

const payload = { executablePath: '/old/claude', version: '1.2.3', authed: true };

describe('agentUnavailableText (D-10)', () => {
	it('is null for a checked agent, including older daemons that omit the field', () => {
		expect(agentUnavailableText(null)).toBeNull();
		expect(agentUnavailableText({})).toBeNull();
		expect(agentUnavailableText({ unavailable: null })).toBeNull();
	});

	it('says "WSL unavailable — <reason>"', () => {
		expect(agentUnavailableText({ unavailable: { kind: 'wsl', reason: 'wsl.exe exited 4' } })).toBe(
			'WSL unavailable — wsl.exe exited 4'
		);
		expect(agentUnavailableText({ unavailable: { kind: 'wsl', reason: '  ' } })).toBe(
			'WSL unavailable'
		);
	});
});

describe('engineFacts (D-10)', () => {
	it('prefers live detection, falling back to the onboarding payload', () => {
		expect(engineFacts(live(), payload)).toEqual({
			authed: true,
			execPath: '/usr/bin/claude',
			version: '2.0.0',
		});
		expect(engineFacts(null, payload)).toEqual({
			authed: true,
			execPath: '/old/claude',
			version: '1.2.3',
		});
	});

	it('reports nothing for an engine detection could not check — not the stale payload', () => {
		const unchecked = live({
			executable_path: 'claude (WSL)',
			version: null,
			authed: null,
			unavailable: { kind: 'wsl', reason: 'timed out' },
		});
		expect(engineFacts(unchecked, payload)).toEqual({
			authed: null,
			execPath: undefined,
			version: null,
		});
	});
});
