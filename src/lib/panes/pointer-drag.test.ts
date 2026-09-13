// Pointer-event drag controller — replaces HTML5 DnD for tab drags because
// WebView2 swallows HTML5 drag events while Tauri's native file-drop handler
// is enabled (Windows).

import { renderHook } from '@testing-library/react';
import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
	DRAG_THRESHOLD_PX,
	type DropTargetHandlers,
	beginPointerDrag,
	isDropTargetRegistered,
	registerDropTarget,
	useDropTarget,
} from './pointer-drag';

/** jsdom has no PointerEvent constructor; a MouseEvent with a pointerId is
 *  indistinguishable to the controller. */
function pointer(type: string, x: number, y: number, pointerId = 1): Event {
	const ev = new MouseEvent(type, { clientX: x, clientY: y, bubbles: true, cancelable: true });
	Object.defineProperty(ev, 'pointerId', { value: pointerId });
	return ev;
}

function target(handlers: Partial<DropTargetHandlers> = {}) {
	const full: DropTargetHandlers = {
		accepts: vi.fn(() => true),
		onOver: vi.fn(),
		onLeave: vi.fn(),
		onDrop: vi.fn(),
		...handlers,
	};
	const { id, unregister } = registerDropTarget(full);
	const el = document.createElement('div');
	el.setAttribute('data-drop-target', id);
	document.body.appendChild(el);
	return { el, handlers: full, unregister };
}

function down(x = 10, y = 10) {
	const source = document.createElement('div');
	document.body.appendChild(source);
	return { button: 0, clientX: x, clientY: y, pointerId: 1, currentTarget: source };
}

let stack: Element[] = [];
const cleanups: Array<() => void> = [];

beforeEach(() => {
	stack = [];
	document.elementsFromPoint = vi.fn(() => stack);
});

afterEach(() => {
	// Always release the module-level "drag in progress" latch.
	window.dispatchEvent(new Event('blur'));
	for (const c of cleanups.splice(0)) c();
	document.body.innerHTML = '';
});

describe('beginPointerDrag', () => {
	it('does not start a drag for movement under the threshold (plain click)', () => {
		const spec = { label: 'tab', onStart: vi.fn(), onEnd: vi.fn() };
		beginPointerDrag(down(), spec);

		window.dispatchEvent(pointer('pointermove', 10 + DRAG_THRESHOLD_PX - 1, 10));
		window.dispatchEvent(pointer('pointerup', 10 + DRAG_THRESHOLD_PX - 1, 10));

		expect(spec.onStart).not.toHaveBeenCalled();
		expect(spec.onEnd).not.toHaveBeenCalled();
	});

	it('drops on the topmost accepting target, handing it the element', () => {
		const t = target();
		cleanups.push(t.unregister);
		const spec = { label: 'tab', onStart: vi.fn(), onEnd: vi.fn() };

		beginPointerDrag(down(), spec);
		stack = [t.el];
		window.dispatchEvent(pointer('pointermove', 60, 40));
		expect(spec.onStart).toHaveBeenCalledTimes(1);
		expect(t.handlers.onOver).toHaveBeenCalledWith(60, 40, t.el);

		window.dispatchEvent(pointer('pointerup', 62, 41));
		expect(t.handlers.onDrop).toHaveBeenCalledWith(62, 41, t.el);
		expect(spec.onEnd).toHaveBeenCalledTimes(1);
	});

	it('falls through a target that declines to the one beneath it', () => {
		const top = target({ accepts: vi.fn(() => false) });
		const below = target();
		cleanups.push(top.unregister, below.unregister);

		beginPointerDrag(down(), { label: 'tab', onStart: vi.fn(), onEnd: vi.fn() });
		stack = [top.el, below.el];
		window.dispatchEvent(pointer('pointermove', 80, 10));
		window.dispatchEvent(pointer('pointerup', 80, 10));

		expect(top.handlers.onDrop).not.toHaveBeenCalled();
		expect(below.handlers.onDrop).toHaveBeenCalledTimes(1);
	});

	it('fires onLeave when the pointer moves off a target', () => {
		const t = target();
		cleanups.push(t.unregister);

		beginPointerDrag(down(), { label: 'tab', onStart: vi.fn(), onEnd: vi.fn() });
		stack = [t.el];
		window.dispatchEvent(pointer('pointermove', 50, 10));
		stack = [];
		window.dispatchEvent(pointer('pointermove', 90, 10));

		expect(t.handlers.onLeave).toHaveBeenCalledTimes(1);
	});

	it('Escape cancels without dropping', () => {
		const t = target();
		cleanups.push(t.unregister);
		const spec = { label: 'tab', onStart: vi.fn(), onEnd: vi.fn() };

		beginPointerDrag(down(), spec);
		stack = [t.el];
		window.dispatchEvent(pointer('pointermove', 50, 10));
		window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
		window.dispatchEvent(pointer('pointerup', 50, 10));

		expect(t.handlers.onDrop).not.toHaveBeenCalled();
		expect(t.handlers.onLeave).toHaveBeenCalled();
		expect(spec.onEnd).toHaveBeenCalledTimes(1);
	});

	it('ignores pointer events from a different pointer', () => {
		const spec = { label: 'tab', onStart: vi.fn(), onEnd: vi.fn() };
		beginPointerDrag(down(), spec);

		window.dispatchEvent(pointer('pointermove', 90, 90, 2));

		expect(spec.onStart).not.toHaveBeenCalled();
	});

	it('ignores non-primary buttons', () => {
		const spec = { label: 'tab', onStart: vi.fn(), onEnd: vi.fn() };
		beginPointerDrag({ ...down(), button: 2 }, spec);

		window.dispatchEvent(pointer('pointermove', 90, 90));

		expect(spec.onStart).not.toHaveBeenCalled();
	});

	it('swallows the click that follows a completed drag, but not later clicks', async () => {
		beginPointerDrag(down(), { label: 'tab', onStart: vi.fn(), onEnd: vi.fn() });
		window.dispatchEvent(pointer('pointermove', 90, 10));
		window.dispatchEvent(pointer('pointerup', 90, 10));

		const onClick = vi.fn();
		document.body.addEventListener('click', onClick);
		document.body.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		expect(onClick).not.toHaveBeenCalled();

		await new Promise((r) => setTimeout(r, 0));
		document.body.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		expect(onClick).toHaveBeenCalledTimes(1);
	});

	it('removes the ghost when the drag ends', () => {
		beginPointerDrag(down(), { label: 'my-tab', onStart: vi.fn(), onEnd: vi.fn() });
		window.dispatchEvent(pointer('pointermove', 90, 10));
		const ghost = Array.from(document.body.children).find((el) => el.textContent === 'my-tab');
		expect(ghost).toBeDefined();

		window.dispatchEvent(pointer('pointerup', 90, 10));
		expect(ghost?.isConnected).toBe(false);
	});
});

describe('useDropTarget', () => {
	// The live bug: drop zones never lit up because React re-ran the hook's
	// effect (Fast Refresh during dev boot) and the cleanup deleted handlers that
	// were only ever registered at first render. StrictMode re-runs effects the
	// same way while keeping state.
	it('stays registered when React re-runs its effect with state kept', () => {
		const { result, unmount } = renderHook(
			() => useDropTarget({ accepts: () => true, onDrop: () => {} }),
			{ wrapper: StrictMode }
		);
		const id = result.current['data-drop-target'];

		expect(isDropTargetRegistered(id)).toBe(true);

		unmount();
		expect(isDropTargetRegistered(id)).toBe(false);
	});

	it('routes calls to the latest handlers without re-registering', () => {
		const first = vi.fn(() => false);
		const second = vi.fn(() => true);
		const { result, rerender } = renderHook(
			({ accepts }) => useDropTarget({ accepts, onDrop: () => {} }),
			{ initialProps: { accepts: first } }
		);
		const id = result.current['data-drop-target'];
		rerender({ accepts: second });
		expect(result.current['data-drop-target']).toBe(id);

		const el = document.createElement('div');
		el.setAttribute('data-drop-target', id);
		document.body.appendChild(el);
		stack = [el];
		const onDrop = vi.fn();
		beginPointerDrag(down(), { label: 'tab', onStart: vi.fn(), onEnd: onDrop });
		window.dispatchEvent(pointer('pointermove', 90, 10));
		window.dispatchEvent(pointer('pointerup', 90, 10));

		expect(first).not.toHaveBeenCalled();
		expect(second).toHaveBeenCalled();
	});
});
