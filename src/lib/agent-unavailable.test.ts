import { describe, expect, it } from 'vitest';

import { agentUnavailableText } from './agent-unavailable';

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
