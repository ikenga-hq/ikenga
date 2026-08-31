// Cross-window lifecycle subscription for detached windows (plans/multi-window
// WP-05). Subscribes to the canonical `window://` topics the Rust
// `WindowRegistry` emits (opened/closed/focus-changed) so a thin window stays
// aware of the others over the shared Rust core — no second TanStack Query
// cache, just the event bus.
//
// This is the substrate proof for Phase-1 verification §3 ("one cross-window
// event flowing in"): the primary spawning/closing a sibling window lands here
// live. (A window may miss its OWN `opened` event — the registry emits it
// during `spawn`, before this listener attaches — but every subsequent
// sibling open/close is observed.)

import { listen, type UnlistenFn } from '@/lib/transport';
import { useEffect, useState } from 'react';

import { WINDOW_TOPICS, type WindowEventEnvelope } from '@ikenga/contract';

export interface WindowLifecycleState {
	/** The most recent `window://` topic seen, or null before the first event. */
	lastTopic: string | null;
	/** The envelope payload of the most recent event. */
	lastEnvelope: WindowEventEnvelope | null;
	/** Count of `window://opened` events observed since mount. */
	openedCount: number;
	/** Count of `window://closed` events observed since mount. */
	closedCount: number;
}

const INITIAL: WindowLifecycleState = {
	lastTopic: null,
	lastEnvelope: null,
	openedCount: 0,
	closedCount: 0,
};

/** Subscribe to the `window://` lifecycle bus. Returns the latest state. */
export function useWindowLifecycle(): WindowLifecycleState {
	const [state, setState] = useState<WindowLifecycleState>(INITIAL);

	useEffect(() => {
		let cancelled = false;
		const unlisteners: UnlistenFn[] = [];
		const topics = [
			WINDOW_TOPICS.opened,
			WINDOW_TOPICS.closed,
			WINDOW_TOPICS.focusChanged,
		] as const;

		void Promise.all(
			topics.map((topic) =>
				listen<WindowEventEnvelope>(topic, (ev) => {
					setState((prev) => ({
						lastTopic: topic,
						lastEnvelope: ev.payload,
						openedCount: topic === WINDOW_TOPICS.opened ? prev.openedCount + 1 : prev.openedCount,
						closedCount: topic === WINDOW_TOPICS.closed ? prev.closedCount + 1 : prev.closedCount,
					}));
				})
			)
		).then((fns) => {
			if (cancelled) {
				for (const f of fns) f();
				return;
			}
			unlisteners.push(...fns);
		});

		return () => {
			cancelled = true;
			for (const f of unlisteners) f();
		};
	}, []);

	return state;
}
