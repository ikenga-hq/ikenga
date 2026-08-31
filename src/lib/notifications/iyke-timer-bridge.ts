// Phase 1 (projects-first-class): OS-notification bridge for the
// `iyke://timer-fired` Tauri event.
//
// The Rust firing loop (`iyke/memory.rs::spawn_timer_fire_loop`) emits
// `iyke://timer-fired` when a pending timer's wall-clock fire_at is
// reached. The FE listener forwards the payload to tauri-plugin-
// notification — same surface acp-notify-bridge uses. Idempotent:
// repeated calls share a single refcounted listener so React StrictMode
// double-mount + HMR don't duplicate notifications.

import { isTauri } from '@/lib/transport';
import { listen } from '@/lib/tauri-cmd';
import {
	isNotificationPermissionGranted,
	requestNotificationPermission,
	sendDesktopNotification,
} from '@/lib/transport/shims';

type Unsubscribe = () => void;

export interface IykeTimerFiredPayload {
	id: string;
	scope: string;
	title: string;
	body: string | null;
	agent_id: string | null;
	fired_at: number;
}

let activeBridge: { unsubscribe: Unsubscribe; refCount: number } | null = null;

export function startIykeTimerBridge(): Unsubscribe {
	// The timer fires as a Tauri event; in a browser session there is no bus
	// to carry it, and subscribing throws at boot.
	if (!isTauri()) {
		console.log('[transport] api/event (iyke timer) is desktop-only — deferred to Wave 2');
		return () => {};
	}
	if (activeBridge) {
		activeBridge.refCount += 1;
		return makeRefCountedUnsubscribe();
	}

	let unlisten: Unsubscribe | null = null;
	let disposed = false;

	void ensureNotificationPermission();

	void listen<IykeTimerFiredPayload>('iyke://timer-fired', (e) => handleTimerFired(e.payload)).then(
		(un) => {
			if (disposed) {
				un();
				return;
			}
			unlisten = un;
		}
	);

	activeBridge = {
		refCount: 1,
		unsubscribe: () => {
			disposed = true;
			if (unlisten) {
				unlisten();
				unlisten = null;
			}
			activeBridge = null;
		},
	};

	return makeRefCountedUnsubscribe();
}

function makeRefCountedUnsubscribe(): Unsubscribe {
	let called = false;
	return () => {
		if (called) return;
		called = true;
		if (!activeBridge) return;
		activeBridge.refCount -= 1;
		if (activeBridge.refCount <= 0) {
			activeBridge.unsubscribe();
		}
	};
}

export function handleTimerFired(payload: IykeTimerFiredPayload): void {
	void fireOsNotification(payload);
}

async function fireOsNotification(payload: IykeTimerFiredPayload): Promise<void> {
	try {
		let granted = await isNotificationPermissionGranted();
		if (!granted) {
			const result = await requestNotificationPermission();
			if (result !== 'granted') return;
		}
		await sendDesktopNotification({
			title: payload.title,
			body: payload.body ?? '',
		});
	} catch (e) {
		console.warn('[iyke-timer-bridge] sendNotification failed', e);
	}
}

async function ensureNotificationPermission(): Promise<void> {
	try {
		const granted = await isNotificationPermissionGranted();
		if (!granted) await requestNotificationPermission();
	} catch (e) {
		console.warn('[iyke-timer-bridge] permission probe failed', e);
	}
}
