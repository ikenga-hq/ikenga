// A tiny general client-side toast. The module-level store lets any code
// (components, handlers, plain TS) raise a toast without a React context, and
// works identically on desktop and in the browser — unlike
// `NotificationToastBridge`, which is driven by server notification events
// that never reach a browser session. `ToastHost`
// (`@/components/ui/toast-host`) renders it; it is mounted once in the shell
// frame beside the status bar.

import { create } from 'zustand';

export type ToastVariant = 'info' | 'error' | 'notice';

export interface ToastAction {
	label: string;
	run: () => void | Promise<void>;
}

export interface ToastOptions {
	label: string;
	variant?: ToastVariant;
	action?: ToastAction;
	/** How long it stays up; `DEFAULT_TOAST_TTL_MS` (longer with an action) when absent. */
	ttlMs?: number;
}

export interface ToastItem {
	/** Unique per toast; the host keys the chip by it so each gets a fresh timer. */
	id: number;
	label: string;
	variant: ToastVariant;
	action?: ToastAction;
	ttlMs?: number;
}

export const DEFAULT_TOAST_TTL_MS = 5_000;
export const DEFAULT_TOAST_ACTION_TTL_MS = 10_000;
/** Beyond this many waiting toasts the oldest queued one is dropped. */
export const MAX_QUEUED_TOASTS = 5;

interface ToastState {
	/** `queue[0]` is on screen; the rest wait their turn. */
	queue: ToastItem[];
	push: (opts: ToastOptions) => number;
	dismiss: (id?: number) => void;
}

let seq = 0;

export const useToastStore = create<ToastState>((set) => ({
	queue: [],
	push: (opts) => {
		const id = ++seq;
		const item: ToastItem = {
			id,
			label: opts.label,
			variant: opts.variant ?? 'info',
			...(opts.action ? { action: opts.action } : {}),
			...(opts.ttlMs ? { ttlMs: opts.ttlMs } : {}),
		};
		set((s) => {
			const next = [...s.queue, item];
			// Keep the visible one (index 0); shed the oldest waiting toast on overflow.
			if (next.length > MAX_QUEUED_TOASTS + 1) next.splice(1, 1);
			return { queue: next };
		});
		return id;
	},
	// No id dismisses whichever toast is showing.
	dismiss: (id) =>
		set((s) => ({
			queue: id === undefined ? s.queue.slice(1) : s.queue.filter((t) => t.id !== id),
		})),
}));

/** Show a toast. Returns its id (for `dismissToast`). */
export function toast(opts: ToastOptions): number {
	return useToastStore.getState().push(opts);
}

/** Dismiss a toast by id, or the one on screen when called without an id. */
export function dismissToast(id?: number): void {
	useToastStore.getState().dismiss(id);
}
