import { cleanup, render } from '@testing-library/react';
import { useCallback, useState } from 'react';
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest';

import { useDismissMenu } from './use-dismiss-menu';

// Model a real browser, not React's test "act" environment: there, a discrete
// event (contextmenu) renders and flushes its effects synchronously while the
// same event is still bubbling — the exact window in which an immediately
// attached `window` listener saw the opening right-click and closed the menu.
const g = globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean };
let prevAct: boolean | undefined;
beforeAll(() => {
	prevAct = g.IS_REACT_ACT_ENVIRONMENT;
	g.IS_REACT_ACT_ENVIRONMENT = false;
});
afterAll(() => {
	g.IS_REACT_ACT_ENVIRONMENT = prevAct;
});

const tick = () => new Promise((r) => setTimeout(r, 10));

function Harness() {
	const [open, setOpen] = useState(false);
	const close = useCallback(() => setOpen(false), []);
	useDismissMenu(open, close);
	return (
		<div
			data-testid="target"
			data-open={open ? 'yes' : 'no'}
			onContextMenu={(e) => {
				e.preventDefault();
				setOpen(true);
			}}
		/>
	);
}

const rightClick = (el: Element) =>
	el.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));

describe('useDismissMenu', () => {
	afterEach(() => cleanup());

	// The opening right-click itself (the bug) only reproduces in a real browser:
	// jsdom doesn't flush the effect mid-dispatch, so a unit test here passes with
	// or without the fix. It was verified live in headless Chrome (menu open 3/3
	// with the fix, closed 3/3 without).

	it('a later click, right-click or Escape closes it', async () => {
		const { getByTestId } = render(<Harness />);
		const el = getByTestId('target');
		for (const dismiss of [
			() => window.dispatchEvent(new MouseEvent('click', { bubbles: true })),
			() => window.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true })),
			() => window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' })),
		]) {
			rightClick(el);
			await tick();
			expect(el.dataset.open).toBe('yes');
			dismiss();
			await tick();
			expect(el.dataset.open).toBe('no');
		}
	});
});
