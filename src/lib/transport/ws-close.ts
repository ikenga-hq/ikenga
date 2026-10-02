// G-ACCESS §3.10 — what the remote client does when the server closes one of
// its WebSockets (`/ws/pty/:id`, `/ws/fs`, `/ws/chat/:id`) on purpose:
//
// - **4401** (`device_revoked`, `epoch_changed`): the credential is dead —
//   revoked, signed out everywhere, password changed, account disabled. A
//   reconnect would be refused, so the socket stops retrying and the page
//   asks for a credential again: the same re-auth / sign-in / pair overlay an
//   RPC `401` opens (`WebRemoteTransport.invoke`).
// - **4403** (`caps_changed`): the credential lives but what it may do
//   changed (a tier change, a routing change, a membership removed). §3.10:
//   "the client reconnects at once with its new caps" — so the socket
//   retries immediately, and the connection banner says access changed until
//   a socket gets back in. One that never does (access removed) stays in
//   that "no access" state rather than counting down a reconnect forever.
//
// Every other code is the ordinary dropped-connection path.

import { connectionStateStore } from './connection-state';

export const WS_CLOSE_REVOKED = 4401;
export const WS_CLOSE_CAPS_CHANGED = 4403;

export type AccessClose = 'revoked' | 'caps_changed';

/** Which access close `code` is, or `null` for an ordinary drop. */
export function accessCloseKind(code: number | undefined): AccessClose | null {
	if (code === WS_CLOSE_REVOKED) return 'revoked';
	if (code === WS_CLOSE_CAPS_CHANGED) return 'caps_changed';
	return null;
}

/**
 * Route an access close: 4401 opens the re-auth overlay, 4403 marks the
 * connection "access changed". Returns the kind so the socket picks its
 * own follow-up (stop vs. reconnect now), or `null` for an ordinary drop.
 */
export function handleAccessClose(code: number | undefined, reason?: string): AccessClose | null {
	const kind = accessCloseKind(code);
	if (kind === 'revoked') {
		connectionStateStore.accessLost('revoked', reason);
		void import('./reauth-store')
			.then(({ useReauthStore }) => useReauthStore.getState().showReauth())
			.catch(() => {});
	} else if (kind === 'caps_changed') {
		connectionStateStore.accessLost('caps_changed', reason);
	}
	return kind;
}

/** At most one immediate 4403 reconnect per socket in this window: a socket
 *  the server keeps closing with 4403 falls back to the normal backoff. */
export const CAPS_RECONNECT_WINDOW_MS = 5_000;

const lastCapsReconnect = new WeakMap<object, number>();

/** Whether `socket` (any stable per-socket object) may reconnect at once
 *  after a 4403 now; records the attempt when it may. */
export function capsReconnectAllowed(socket: object, now: number = Date.now()): boolean {
	const last = lastCapsReconnect.get(socket);
	if (last !== undefined && now - last < CAPS_RECONNECT_WINDOW_MS) return false;
	lastCapsReconnect.set(socket, now);
	return true;
}
