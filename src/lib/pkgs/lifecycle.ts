// Pkg-kernel lifecycle subscription helper.
//
// The Rust supervisor (src-tauri/src/pkg/lifecycle.rs) emits a single
// `pkg://lifecycle` Tauri event on every state transition for any pkg with
// `mcp[].lifecycle = "long-lived"`. This module translates that stream into
// a React hook that mirrors the previous `*://crashed` per-pkg subscriptions
// the FE used to roll on top of the now-deleted `video_servers.rs`.
//
// State surface is collapsed 3-state — booting / ready / error{reason} —
// since that's all the panes render. The full Rust enum is still available
// via `pkgKernelStatus` for debugging.

import { useEffect, useState } from 'react';
import { listen, type UnlistenFn } from '@/lib/transport';
import { pkgKernelStatus } from '@/lib/tauri-cmd';

export type PkgState = { kind: 'booting' } | { kind: 'ready' } | { kind: 'error'; reason: string };

interface LifecycleEvent {
	pkgId: string;
	kind: 'booting' | 'ready' | 'error';
	reason?: string;
}

const LIFECYCLE_CHANNEL = 'pkg://lifecycle';

function eventToState(ev: LifecycleEvent): PkgState {
	switch (ev.kind) {
		case 'booting':
			return { kind: 'booting' };
		case 'ready':
			return { kind: 'ready' };
		case 'error':
			return { kind: 'error', reason: ev.reason ?? 'unknown error' };
	}
}

/**
 * Translate the supervisor's status snapshot for a given pkg into the
 * collapsed FE state. Mirrors the Rust `emit_lifecycle` mapping so initial
 * pulls and subscription deltas line up.
 *
 * `state` strings come from `State::label()`: spawning / running / crashed
 * / blocked / parked / shuttingdown.
 */
function snapshotToState(state: string, lastErr: string | null): PkgState {
	switch (state) {
		case 'spawning':
			return { kind: 'booting' };
		case 'running':
			return { kind: 'ready' };
		case 'crashed':
		case 'blocked':
		case 'parked':
			return { kind: 'error', reason: lastErr ?? 'unknown error' };
		default:
			return { kind: 'booting' };
	}
}

interface SupervisorEntry {
	pkg_id: string;
	state: string;
	last_err: string | null;
}

interface SupervisorRegistry {
	count?: number;
	entries?: SupervisorEntry[];
}

/**
 * One-shot pull of the current state for a pkg from `pkg_kernel_status`.
 * Used to seed `usePkgLifecycle` before the listen subscription has fired.
 * Returns null if the pkg is not supervised here (per-call lifecycle pkgs,
 * or pkg not installed).
 */
export async function getPkgLifecycleSnapshot(pkgId: string): Promise<PkgState | null> {
	const status = await pkgKernelStatus();
	const supervisor = status.registries?.['sidecar_supervisor'] as SupervisorRegistry | undefined;
	const entry = supervisor?.entries?.find((e) => e.pkg_id === pkgId);
	if (!entry) return null;
	return snapshotToState(entry.state, entry.last_err ?? null);
}

/**
 * Subscribe to lifecycle transitions for a single pkg. Returns an unlisten
 * fn. Filtering happens in JS — there's only one channel for the whole app.
 */
export function subscribePkgLifecycle(
	pkgId: string,
	cb: (state: PkgState) => void
): Promise<UnlistenFn> {
	return listen<LifecycleEvent>(LIFECYCLE_CHANNEL, (ev) => {
		if (ev.payload.pkgId !== pkgId) return;
		cb(eventToState(ev.payload));
	});
}

/**
 * React hook: latest supervised lifecycle state for a pkg. Initial value is
 * `booting` until the pkg_kernel_status pull resolves; switches to whatever
 * the supervisor reports and stays in sync via the broadcast subscription.
 *
 * Accepts a nullable pkgId so call-sites can short-circuit cleanly when
 * they don't yet know which pkg to watch.
 */
export function usePkgLifecycle(pkgId: string | null): PkgState {
	const [state, setState] = useState<PkgState>({ kind: 'booting' });

	useEffect(() => {
		if (!pkgId) return;
		let cancelled = false;
		let unlisten: UnlistenFn | null = null;

		void (async () => {
			const snap = await getPkgLifecycleSnapshot(pkgId).catch(() => null);
			if (cancelled) return;
			if (snap) setState(snap);

			unlisten = await subscribePkgLifecycle(pkgId, (next) => {
				if (!cancelled) setState(next);
			});
			if (cancelled) {
				unlisten();
				unlisten = null;
			}
		})();

		return () => {
			cancelled = true;
			unlisten?.();
		};
	}, [pkgId]);

	return state;
}
