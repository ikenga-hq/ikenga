// Pointer-event drag controller for in-page drags: tab move / split / reorder
// (pane strip, dock) and list reordering.
//
// Why not HTML5 drag-and-drop: on Windows, WebView2 never delivers HTML5 drag
// events to the page while Tauri's native drag-drop handler is enabled — Tauri
// documents disabling it as "required to use HTML5 drag and drop APIs on the
// frontend on Windows". That same handler is the only source of a dropped OS
// file's real path (`lib/dnd/os-file-drop.ts`), so rather than trade one for
// the other, in-page drags run on pointer events, which no native handler
// intercepts on any platform.
//
// Model:
//   • A source calls `beginPointerDrag` from `onPointerDown`. Nothing happens
//     until the pointer travels `DRAG_THRESHOLD_PX`, so plain clicks,
//     middle-clicks and context menus behave exactly as before.
//   • Drop targets mark an element with the props from `useDropTarget`. While
//     dragging, the topmost marked element under the pointer whose `accepts`
//     returns true receives `onOver` / `onLeave` / `onDrop`, along with that
//     element (for rect math). A target that declines lets the next one
//     beneath it take the drag.
//   • Escape, window blur or `pointercancel` abort without dropping; the click
//     the browser fires after a completed drag is swallowed so the source tab
//     doesn't also activate.

import { useEffect, useRef, useState } from 'react';

export const DRAG_THRESHOLD_PX = 4;

const TARGET_ATTR = 'data-drop-target';

export interface DropTargetHandlers {
	/** Whether this target takes the current drag at client (x, y). */
	accepts: (x: number, y: number, el: Element) => boolean;
	onOver?: (x: number, y: number, el: Element) => void;
	onLeave?: () => void;
	onDrop: (x: number, y: number, el: Element) => void;
}

export interface PointerDragSpec {
	/** Text shown in the floating ghost that follows the pointer. */
	label: string;
	/** Called once the threshold is crossed — publish the drag payload here. */
	onStart: () => void;
	/** Called when the drag finishes, dropped or not. Always after `onDrop`. */
	onEnd: () => void;
}

/** The subset of a pointer event `beginPointerDrag` reads. */
export interface PointerDownLike {
	button: number;
	clientX: number;
	clientY: number;
	pointerId: number;
	currentTarget: EventTarget | null;
}

export type DropTargetProps = { [TARGET_ATTR]: string };

const targets = new Map<string, DropTargetHandlers>();
let nextTargetId = 0;
let dragInProgress = false;

function mintTargetId(): string {
	nextTargetId += 1;
	return `dt-${nextTargetId}`;
}

/** Whether a drop target id currently has handlers registered. */
export function isDropTargetRegistered(id: string): boolean {
	return targets.has(id);
}

/** Register a drop target outside React. Returns the attribute value to put on
 *  the element(s) and an unregister function. */
export function registerDropTarget(handlers: DropTargetHandlers): {
	id: string;
	unregister: () => void;
} {
	const id = mintTargetId();
	targets.set(id, handlers);
	return { id, unregister: () => targets.delete(id) };
}

/**
 * Mark element(s) as a drop target. Spread the returned props onto them — the
 * same props may go on several elements that should behave as one target.
 * Handlers are read through a ref, so fresh closures every render are fine and
 * never re-register.
 */
export function useDropTarget(handlers: DropTargetHandlers): DropTargetProps {
	const handlersRef = useRef(handlers);
	handlersRef.current = handlers;
	const [id] = useState(mintTargetId);
	// Register in the effect's setup, not once at first render. React re-runs
	// effects while keeping state (Fast Refresh on any module update — which a
	// Vite `invalidate` triggers during dev boot — StrictMode, <Activity>). A
	// registration made only at first render would be deleted by the first
	// cleanup and never restored, leaving an element that is under the pointer
	// but has no handlers: drop zones that never light up.
	useEffect(() => {
		targets.set(id, {
			accepts: (x, y, el) => handlersRef.current.accepts(x, y, el),
			onOver: (x, y, el) => handlersRef.current.onOver?.(x, y, el),
			onLeave: () => handlersRef.current.onLeave?.(),
			onDrop: (x, y, el) => handlersRef.current.onDrop(x, y, el),
		});
		return () => {
			targets.delete(id);
		};
	}, [id]);
	return { [TARGET_ATTR]: id };
}

type Hit = { id: string; handlers: DropTargetHandlers; el: Element };

function targetAt(x: number, y: number): Hit | null {
	// `elementsFromPoint` is topmost-first and skips `pointer-events: none`, so
	// the ghost and inactive overlays never shadow a real target.
	const stack = document.elementsFromPoint?.(x, y) ?? [];
	for (const el of stack) {
		const id = el.getAttribute(TARGET_ATTR);
		if (!id) continue;
		const handlers = targets.get(id);
		if (handlers?.accepts(x, y, el)) return { id, handlers, el };
	}
	return null;
}

function createGhost(label: string): HTMLDivElement {
	const el = document.createElement('div');
	el.setAttribute('aria-hidden', 'true');
	el.textContent = label;
	Object.assign(el.style, {
		position: 'fixed',
		left: '0',
		top: '0',
		zIndex: '2147483647',
		pointerEvents: 'none',
		maxWidth: '220px',
		overflow: 'hidden',
		textOverflow: 'ellipsis',
		whiteSpace: 'nowrap',
		padding: '3px 10px',
		borderRadius: '6px',
		font: '500 12px/1.4 var(--font-sans, system-ui, sans-serif)',
		color: 'var(--popover-foreground, #fff)',
		background: 'var(--popover, #222)',
		border: '1px solid var(--border, rgba(255,255,255,0.15))',
		boxShadow: '0 4px 12px rgba(0,0,0,0.3)',
		opacity: '0.92',
	} as Partial<CSSStyleDeclaration>);
	document.body.appendChild(el);
	return el;
}

function suppressNextClick(): void {
	const swallow = (ev: MouseEvent) => {
		ev.stopPropagation();
		ev.preventDefault();
	};
	window.addEventListener('click', swallow, { capture: true, once: true });
	// Only the click generated by this pointerup; never a later, real one.
	setTimeout(() => window.removeEventListener('click', swallow, true), 0);
}

/** Start tracking a potential drag from a `pointerdown`. No-op for non-primary
 *  buttons or while another drag is in progress. */
export function beginPointerDrag(down: PointerDownLike, spec: PointerDragSpec): void {
	if (down.button !== 0 || dragInProgress) return;
	dragInProgress = true;

	const source = down.currentTarget instanceof Element ? down.currentTarget : null;
	let dragging = false;
	let over: Hit | null = null;
	let ghost: HTMLDivElement | null = null;
	const prevCursor = document.body.style.cursor;
	const prevUserSelect = document.body.style.userSelect;

	const isOurs = (ev: PointerEvent) => ev.pointerId === down.pointerId;

	const track = (x: number, y: number) => {
		if (ghost) ghost.style.transform = `translate(${x + 14}px, ${y + 14}px)`;
		const hit = targetAt(x, y);
		if (over && over.id !== hit?.id) over.handlers.onLeave?.();
		over = hit;
		hit?.handlers.onOver?.(x, y, hit.el);
		document.body.style.cursor = hit ? 'grabbing' : 'no-drop';
	};

	const onMove = (ev: PointerEvent) => {
		if (!isOurs(ev)) return;
		if (!dragging) {
			const dx = ev.clientX - down.clientX;
			const dy = ev.clientY - down.clientY;
			if (Math.hypot(dx, dy) < DRAG_THRESHOLD_PX) return;
			dragging = true;
			// Capture keeps pointer events flowing while the cursor crosses
			// iframes or leaves the window mid-drag.
			try {
				source?.setPointerCapture?.(down.pointerId);
			} catch {
				/* element may have unmounted */
			}
			document.body.style.userSelect = 'none';
			ghost = createGhost(spec.label);
			spec.onStart();
		}
		ev.preventDefault();
		track(ev.clientX, ev.clientY);
	};

	const onUp = (ev: PointerEvent) => {
		if (!isOurs(ev)) return;
		if (dragging) {
			const hit = targetAt(ev.clientX, ev.clientY);
			if (over && over.id !== hit?.id) over.handlers.onLeave?.();
			over = null;
			hit?.handlers.onDrop(ev.clientX, ev.clientY, hit.el);
			suppressNextClick();
		}
		finish();
	};

	const onCancel = () => {
		if (dragging) over?.handlers.onLeave?.();
		over = null;
		finish();
	};

	const onPointerCancel = (ev: PointerEvent) => {
		if (isOurs(ev)) onCancel();
	};

	const onKey = (ev: KeyboardEvent) => {
		if (ev.key !== 'Escape') return;
		ev.preventDefault();
		ev.stopPropagation();
		onCancel();
	};

	function finish() {
		window.removeEventListener('pointermove', onMove);
		window.removeEventListener('pointerup', onUp);
		window.removeEventListener('pointercancel', onPointerCancel);
		window.removeEventListener('keydown', onKey, true);
		window.removeEventListener('blur', onCancel);
		ghost?.remove();
		ghost = null;
		if (dragging) {
			try {
				source?.releasePointerCapture?.(down.pointerId);
			} catch {
				/* already released */
			}
			document.body.style.cursor = prevCursor;
			document.body.style.userSelect = prevUserSelect;
			spec.onEnd();
		}
		dragInProgress = false;
	}

	window.addEventListener('pointermove', onMove);
	window.addEventListener('pointerup', onUp);
	window.addEventListener('pointercancel', onPointerCancel);
	window.addEventListener('keydown', onKey, true);
	window.addEventListener('blur', onCancel);
}
