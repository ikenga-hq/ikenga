// WP-55 review — the macOS menu bar rebuild is one serialized queue (a stale
// build never lands last; the replaced menu's resources are closed), and a
// native-menu accelerator honours its binding's `when`.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { DEDUPE_WINDOW_MS } from '@/lib/keymap/commands';
import { acceleratorBlocked, type BuiltMenu, createRebuildQueue } from './native-menu';

function deferred<T>() {
	let resolve!: (v: T) => void;
	const promise = new Promise<T>((r) => {
		resolve = r;
	});
	return { promise, resolve };
}

function fakeBuild(label: string, log: string[]): BuiltMenu {
	return {
		apply: async () => {
			log.push(`apply ${label}`);
		},
		resources: [{ close: async () => void log.push(`close ${label}`) }],
	};
}

describe('createRebuildQueue', () => {
	it('coalesces requests made together into one build', async () => {
		const log: string[] = [];
		let n = 0;
		const q = createRebuildQueue(async () => fakeBuild(String(++n), log));
		q.request();
		q.request();
		q.request();
		await q.settled();
		expect(log).toEqual(['apply 1']);
	});

	it('never applies a build a newer request superseded, and applies the newest last', async () => {
		const log: string[] = [];
		const first = deferred<BuiltMenu>();
		let calls = 0;
		const q = createRebuildQueue(() => {
			calls++;
			return calls === 1 ? first.promise : Promise.resolve(fakeBuild(String(calls), log));
		});
		q.request();
		await Promise.resolve();
		// A keymap publish lands while the model-change build is in flight.
		q.request();
		first.resolve(fakeBuild('stale', log));
		await q.settled();
		expect(log).toEqual(['close stale', 'apply 2']);
		expect(calls).toBe(2);
	});

	it('closes the previous menu’s resources only after the new one is applied', async () => {
		const log: string[] = [];
		let n = 0;
		const q = createRebuildQueue(async () => fakeBuild(String(++n), log));
		q.request();
		await q.settled();
		q.request();
		await q.settled();
		expect(log).toEqual(['apply 1', 'apply 2', 'close 1']);
	});

	it('keeps going after a failed build', async () => {
		const log: string[] = [];
		const onError = vi.fn();
		let n = 0;
		const q = createRebuildQueue(
			async () => {
				n++;
				if (n === 1) throw new Error('boom');
				return fakeBuild(String(n), log);
			},
			{ onError }
		);
		q.request();
		await q.settled();
		q.request();
		await q.settled();
		expect(onError).toHaveBeenCalledTimes(1);
		expect(log).toEqual(['apply 2']);
	});
});

describe('acceleratorBlocked — `!inputFocus` through the native menu', () => {
	afterEach(() => {
		document.body.innerHTML = '';
	});

	function press(target: Element) {
		const event = new KeyboardEvent('keydown', { key: 'j', metaKey: true, bubbles: true });
		Object.defineProperty(event, 'target', { value: target });
		return { event, at: 1_000 };
	}

	it('blocks the accelerator of a `!inputFocus` command pressed while typing', () => {
		const input = document.createElement('input');
		document.body.appendChild(input);
		input.focus();
		// companion.toggle = mod+j, when `!inputFocus`.
		expect(acceleratorBlocked('companion.toggle', press(input), 1_050)).toBe(true);
	});

	it('lets the same accelerator through outside a text input', () => {
		const div = document.createElement('div');
		document.body.appendChild(div);
		expect(acceleratorBlocked('companion.toggle', press(div), 1_050)).toBe(false);
	});

	it('never blocks a mouse click (no matching keydown, or a stale one)', () => {
		const input = document.createElement('input');
		document.body.appendChild(input);
		input.focus();
		expect(acceleratorBlocked('companion.toggle', null)).toBe(false);
		expect(acceleratorBlocked('companion.toggle', press(input), 1_000 + DEDUPE_WINDOW_MS + 1)).toBe(false);
		// A keydown for another key is not this item's accelerator.
		const other = new KeyboardEvent('keydown', { key: 'k', metaKey: true });
		Object.defineProperty(other, 'target', { value: input });
		expect(acceleratorBlocked('companion.toggle', { event: other, at: 1_000 }, 1_050)).toBe(false);
	});
});
