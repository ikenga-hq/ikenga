import { describe, expect, it, vi } from 'vitest';
import { beforeUnloadPrompt, installUnloadGuard } from './unload-guard';

function harness(opts: { browser: boolean; count: number }) {
	let count = opts.count;
	const listeners = new Set<() => void>();
	const target = { addEventListener: vi.fn(), removeEventListener: vi.fn() };
	const off = installUnloadGuard({
		isBrowser: () => opts.browser,
		terminalCount: () => count,
		subscribe: (l) => {
			listeners.add(l);
			return () => listeners.delete(l);
		},
		target,
	});
	return {
		target,
		off,
		set(n: number) {
			count = n;
			for (const l of [...listeners]) l();
		},
	};
}

describe('installUnloadGuard', () => {
	it('registers nothing on the desktop, even with terminals open', () => {
		const h = harness({ browser: false, count: 3 });
		expect(h.target.addEventListener).not.toHaveBeenCalled();
		h.set(2);
		expect(h.target.addEventListener).not.toHaveBeenCalled();
	});

	it('registers nothing in a browser while no terminal is open', () => {
		const h = harness({ browser: true, count: 0 });
		expect(h.target.addEventListener).not.toHaveBeenCalled();
	});

	it('registers beforeunload in a browser once a terminal opens, and drops it when the last closes', () => {
		const h = harness({ browser: true, count: 0 });
		h.set(1);
		expect(h.target.addEventListener).toHaveBeenCalledTimes(1);
		expect(h.target.addEventListener).toHaveBeenCalledWith('beforeunload', beforeUnloadPrompt);
		h.set(2); // more terminals: still one registration
		expect(h.target.addEventListener).toHaveBeenCalledTimes(1);
		h.set(0);
		expect(h.target.removeEventListener).toHaveBeenCalledWith('beforeunload', beforeUnloadPrompt);
	});

	it('registers immediately when terminals already exist at install', () => {
		const h = harness({ browser: true, count: 2 });
		expect(h.target.addEventListener).toHaveBeenCalledWith('beforeunload', beforeUnloadPrompt);
		h.off();
		expect(h.target.removeEventListener).toHaveBeenCalledWith('beforeunload', beforeUnloadPrompt);
	});

	it('the handler prompts (preventDefault)', () => {
		const e = new Event('beforeunload', { cancelable: true }) as BeforeUnloadEvent;
		beforeUnloadPrompt(e);
		expect(e.defaultPrevented).toBe(true);
	});
});
