// scope — where the `equipment` step may touch `.claude/`. Written under
// DEC-50; not run in the WP-38 round-2 fix pass.

import { describe, expect, it } from 'vitest';

import { effectiveOnboardingScope, onboardingClaudeRoot } from './scope';

describe('effectiveOnboardingScope', () => {
	it('falls back to personal without a project root', () => {
		expect(effectiveOnboardingScope(null, null)).toBe('personal');
		expect(effectiveOnboardingScope('project', null)).toBe('personal');
		expect(effectiveOnboardingScope(null, '/repo')).toBe('project');
		expect(effectiveOnboardingScope('personal', '/repo')).toBe('personal');
	});
});

describe('onboardingClaudeRoot', () => {
	it('never targets the home directory implicitly', () => {
		// "Start empty" / rootless default project, switch untouched.
		expect(onboardingClaudeRoot(null, null, '/home/me')).toBeNull();
		// A stale Project pick with no root is not a personal choice either.
		expect(onboardingClaudeRoot('project', null, '/home/me')).toBeNull();
	});

	it('targets the home directory only when Personal was picked', () => {
		expect(onboardingClaudeRoot('personal', null, '/home/me')).toBe('/home/me');
		expect(onboardingClaudeRoot('personal', '/repo', '/home/me')).toBe('/home/me');
		// Home not resolved yet: nowhere, not a guess.
		expect(onboardingClaudeRoot('personal', null, undefined)).toBeNull();
	});

	it('targets the project root under project scope', () => {
		expect(onboardingClaudeRoot(null, '/repo', '/home/me')).toBe('/repo');
		expect(onboardingClaudeRoot('project', '/repo', '/home/me')).toBe('/repo');
	});
});
