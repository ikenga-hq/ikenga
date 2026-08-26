// Workspace-level hook that bridges screenshot events with the DOM.
//
// Two events to handle:
//  1. `screenshot://request` — Rust capture helper asking us to render a
//     target to PNG. We capture, base64-encode, post back via
//     `screenshot_capture_done`. The Rust side awaits a oneshot keyed on
//     `request_id` and writes the bytes to disk.
//  2. `screenshot://shortcut` — global shortcut fired in Rust. We resolve
//     "focused pane" here (instead of mirroring usePaneStore on the Rust
//     side just for this) and invoke the matching Tauri command.

import { useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { isTauri } from '@/lib/transport';

import {
	capturePane,
	captureWindow,
	FE_CLONE_NODE_CEILING,
	findPaneElement,
	paneHasOwnOverflow,
	subtreeNodeCount,
} from '@/lib/screenshot';
import { useIykeActivity } from '@/lib/iyke/activity-store';
import { usePaneStore } from '@/lib/panes/pane-store';
import { screenshotPane, screenshotWindow } from '@/lib/tauri-cmd';

interface RequestPayload {
	request_id: string;
	kind: 'window' | 'pane';
	pane_id: string | null;
	/** When true, the Rust side requires the FE `modern-screenshot` clone
	 *  even for an in-viewport pane (native crop unavailable/unreliable on
	 *  this compositor). Absent/false ⇒ the FE decides native vs clone. */
	force_fe?: boolean;
}

interface ShortcutPayload {
	kind: 'window' | 'pane-focused';
}

export function useScreenshotListener() {
	useEffect(() => {
		// Native capture has no browser path, and the `listen()` below throws
		// at mount without a Tauri host.
		if (!isTauri()) {
			console.log('[transport] api/event (screenshot) is desktop-only — deferred to Wave 2');
			return;
		}
		let alive = true;
		const unlisteners: Array<() => void> = [];

		void (async () => {
			const offReq = await listen<RequestPayload>('screenshot://request', async (e) => {
				const { request_id, kind, pane_id, force_fe } = e.payload;
				const scope = kind === 'pane' && pane_id ? pane_id : 'window';
				const actId = useIykeActivity.getState().begin({ kind: 'screenshot', scope });

				const reportFailure = async (message: string) => {
					try {
						await invoke('screenshot_capture_failed', { args: { request_id, message } });
					} catch (reportErr) {
						// eslint-disable-next-line no-console
						console.warn('[screenshot] failed to report failure', reportErr);
					}
				};

				try {
					// Pane: prefer the native window-crop (cheap, can't stall the
					// renderer). Only fall to the synchronous modern-screenshot
					// clone when the pane has its own off-screen content to
					// capture, or when Rust forces it (native crop unavailable).
					if (kind === 'pane') {
						const el = findPaneElement(pane_id ?? '');
						if (!el) {
							await reportFailure(`pane not found: ${pane_id}`);
							return;
						}
						if (!force_fe && !paneHasOwnOverflow(el)) {
							const r = el.getBoundingClientRect();
							await invoke('screenshot_capture_native_crop', {
								args: { request_id, rect: [r.left, r.top, r.width, r.height] },
							});
							return;
						}
						// FE clone path. Refuse a pathological subtree rather than
						// risk aborting the WebKitGTK renderer.
						const nodes = subtreeNodeCount(el);
						if (nodes > FE_CLONE_NODE_CEILING) {
							await reportFailure(
								`pane DOM too large to capture in-app (${nodes} nodes); use the window screenshot instead`
							);
							return;
						}
					}

					const out = kind === 'window' ? await captureWindow() : await capturePane(pane_id ?? '');
					await invoke('screenshot_capture_done', {
						args: {
							request_id,
							png_base64: out.base64,
							width: out.width,
							height: out.height,
						},
					});
				} catch (err) {
					// Report failure so the Rust oneshot resolves immediately
					// instead of waiting out the 60s capture timeout.
					// eslint-disable-next-line no-console
					console.warn('[screenshot] capture failed', err);
					await reportFailure(err instanceof Error ? err.message : String(err));
				} finally {
					useIykeActivity.getState().end(actId);
				}
			});
			if (!alive) {
				offReq();
				return;
			}
			unlisteners.push(offReq);

			const offShortcut = await listen<ShortcutPayload>('screenshot://shortcut', async (e) => {
				try {
					if (e.payload.kind === 'window') {
						await screenshotWindow();
					} else {
						const focusedId = usePaneStore.getState().focusedId;
						if (focusedId) {
							await screenshotPane(focusedId);
						} else {
							// No focused pane — fall back to a window capture so the
							// shortcut isn't a silent no-op.
							await screenshotWindow();
						}
					}
				} catch (err) {
					// eslint-disable-next-line no-console
					console.warn('[screenshot] shortcut handler failed', err);
				}
			});
			if (!alive) {
				offShortcut();
				return;
			}
			unlisteners.push(offShortcut);
		})();

		return () => {
			alive = false;
			for (const off of unlisteners) off();
		};
	}, []);
}
