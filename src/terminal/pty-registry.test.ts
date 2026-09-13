// Single-flight PTY acquisition — the guard against restored tabs spawning
// several PTYs (rehydrate auto-resume + SingleTerminal effect re-runs racing).

import { describe, expect, it, vi } from 'vitest';

import type { Pty } from './pty-bridge';
import { acquirePty, disposePty, getPty } from './pty-registry';

vi.mock('./pty-output-buffer', () => ({ detachCapture: vi.fn() }));

function fakePty(id: string): Pty {
	return { id, dispose: vi.fn().mockResolvedValue(undefined) } as unknown as Pty;
}

/** An `open` whose resolution the test controls. */
function deferredOpen(pty: Pty) {
	let resolve!: (p: Pty) => void;
	const open = vi.fn(() => new Promise<Pty>((r) => (resolve = r)));
	return { open, land: () => resolve(pty) };
}

describe('acquirePty', () => {
	it('opens once for concurrent callers and hands them the same PTY', async () => {
		const pty = fakePty('p1');
		const { open, land } = deferredOpen(pty);
		const onOpened = vi.fn();

		const a = acquirePty('s-concurrent', open, onOpened);
		const b = acquirePty('s-concurrent', open, onOpened);
		land();

		expect(await a).toBe(pty);
		expect(await b).toBe(pty);
		expect(open).toHaveBeenCalledTimes(1);
		expect(onOpened).toHaveBeenCalledTimes(1);
		expect(getPty('s-concurrent')).toBe(pty);
	});

	it('returns the registered PTY without opening again', async () => {
		const pty = fakePty('p2');
		await acquirePty('s-registered', async () => pty);
		const open = vi.fn(async () => fakePty('never'));

		expect(await acquirePty('s-registered', open)).toBe(pty);
		expect(open).not.toHaveBeenCalled();
	});

	it('opens a fresh PTY after the previous one was disposed', async () => {
		await acquirePty('s-respawn', async () => fakePty('old'));
		disposePty('s-respawn');
		const next = fakePty('new');

		expect(await acquirePty('s-respawn', async () => next)).toBe(next);
	});

	it('lets a failed open be retried', async () => {
		await expect(
			acquirePty('s-retry', async () => {
				throw new Error('spawn failed');
			})
		).rejects.toThrow('spawn failed');
		const pty = fakePty('p3');

		expect(await acquirePty('s-retry', async () => pty)).toBe(pty);
	});

	it('kills a PTY that lands after its session was disposed', async () => {
		const pty = fakePty('orphan');
		const { open, land } = deferredOpen(pty);
		const onOpened = vi.fn();

		const pending = acquirePty('s-orphan', open, onOpened);
		disposePty('s-orphan');
		land();

		await expect(pending).rejects.toThrow('disposed while its PTY was opening');
		expect(pty.dispose).toHaveBeenCalled();
		expect(onOpened).not.toHaveBeenCalled();
		expect(getPty('s-orphan')).toBeUndefined();
	});
});
