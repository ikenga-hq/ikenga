// WP-67 — UI session numbering: stable once given, and a resumed
// conversation keeps its number (D-09: *Resume session 2* → "session 2").

import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@/lib/transport', () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock('@/lib/iyke/client', () => ({ iykeFetch: vi.fn(async () => ({ ok: false })) }));

import { useTerminalStore } from '@/terminal/session-store';
import { __resetSessionNumbersForTests, aliasSessionNumber, sessionName, sessionNumber } from './seat-sessions';

beforeEach(() => {
	__resetSessionNumbersForTests();
	useTerminalStore.setState({ tabs: [] });
});

describe('session numbers', () => {
	it('are stable once given', () => {
		const a = sessionNumber('term-a');
		const b = sessionNumber('term-b');
		expect(b).toBe(a + 1);
		expect(sessionNumber('term-a')).toBe(a);
	});

	it('a resumed conversation keeps its number, even if the new ref was numbered first', () => {
		const two = sessionNumber('term-old');
		sessionNumber('term-new');
		aliasSessionNumber('term-new', 'term-old');
		expect(sessionName('term-new')).toBe(`session ${two}`);
		aliasSessionNumber('run-new', 'term-old');
		expect(sessionNumber('run-new')).toBe(two);
	});
});
