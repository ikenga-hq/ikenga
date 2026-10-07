import { useEffect } from 'react';

/**
 * Close an open menu on the next click, right-click or Escape anywhere.
 *
 * The listeners are attached on the NEXT tick, not in the effect body: React
 * flushes effects synchronously for discrete events such as `contextmenu`, so
 * a listener added straight away saw the very right-click that opened the menu
 * bubble up to `window` and closed it at once — the terminal's right-click menu
 * (Copy / Paste) never stayed open.
 */
export function useDismissMenu(open: boolean, close: () => void): void {
	useEffect(() => {
		if (!open) return;
		const onKey = (e: KeyboardEvent) => {
			if (e.key === 'Escape') close();
		};
		const t = setTimeout(() => {
			window.addEventListener('click', close);
			window.addEventListener('contextmenu', close);
			window.addEventListener('keydown', onKey);
		}, 0);
		return () => {
			clearTimeout(t);
			window.removeEventListener('click', close);
			window.removeEventListener('contextmenu', close);
			window.removeEventListener('keydown', onKey);
		};
	}, [open, close]);
}
