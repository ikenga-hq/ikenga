import { create } from 'zustand';
import { normalizePairCode } from '@/lib/access/pair-code';
import { isDeviceSession } from './device-session';
import { signInWithPassword } from './t1-session';

/**
 * Which credential the overlay asks for:
 * - `auto` — the tier's own: the T0 token, or the T1 username + password
 *   (WP-20);
 * - `pair` — "Pair this device" (G-ACCESS §3.12, WP-74b): a code from the
 *   computer, run at `/remote/pair`. A paired device whose grant died (revoked,
 *   idle-expired) lands here.
 */
export type ReauthMode = 'auto' | 'pair';

/**
 * Why the dialog is open. `expired`: this tab had a token and the daemon
 * refused it (it restarted and minted a new one). `first-visit`: this tab has
 * never had one, so there is nothing to have expired.
 *
 * T1 (multi-user, docs/remote/principal-contract.md §2.4) adds a
 * username/password mode to this same dialog, chosen from the tier that
 * `/api/health` reports. Keep new sign-in modes here; don't add a second overlay.
 */
export type ReauthReason = 'expired' | 'first-visit';

interface ReauthStore {
	isOpen: boolean;
	reason: ReauthReason;
	mode: ReauthMode;
	setMode: (mode: ReauthMode) => void;
	showReauth: (reason?: ReauthReason) => void;
	/** Open straight into "Pair this device" (a browser with no credential). */
	showPair: () => void;
	/** Leave for `/remote/pair` with a code (`#c=` keeps it out of the server's
	 *  logs). `false` when the code doesn't parse. */
	pairWithCode: (code: string) => boolean;
	hideReauth: () => void;
	tokenInput: string;
	setTokenInput: (val: string) => void;
	errorMsg: string | null;
	setErrorMsg: (msg: string | null) => void;
	reconnect: (token: string) => Promise<boolean>;
	/** T1 (G-PRINCIPAL §2.4): sign in at `/auth/login`. The broker sets the
	 *  session cookie and the page reloads into a normal boot, like
	 *  `reconnect`. */
	signIn: (username: string, password: string) => Promise<boolean>;
}

export const useReauthStore = create<ReauthStore>((set) => ({
	isOpen: false,
	reason: 'expired',
	mode: 'auto',
	tokenInput: '',
	errorMsg: null,
	setMode: (mode) => set({ mode, errorMsg: null }),
	showReauth: (reason = 'expired') =>
		set({
			isOpen: true,
			reason,
			errorMsg: null,
			mode: isDeviceSession() ? 'pair' : 'auto',
		}),
	showPair: () => set({ isOpen: true, errorMsg: null, mode: 'pair' }),
	pairWithCode: (code: string) => {
		const norm = normalizePairCode(code);
		if (!norm) {
			set({ errorMsg: 'A code is 6 letters and numbers, like K7P-42Q.' });
			return false;
		}
		if (typeof window !== 'undefined') {
			window.location.assign(`/remote/pair#c=${norm}`);
		}
		return true;
	},
	hideReauth: () => set({ isOpen: false, errorMsg: null }),
	setTokenInput: (val) => set({ tokenInput: val }),
	setErrorMsg: (msg) => set({ errorMsg: msg }),
	reconnect: async (newToken: string) => {
		if (!newToken.trim()) {
			set({ errorMsg: 'Please enter a valid token.' });
			return false;
		}
		try {
			const res = await fetch('/api/rpc', {
				method: 'POST',
				headers: {
					'Content-Type': 'application/json',
					Authorization: `Bearer ${newToken.trim()}`,
				},
				body: JSON.stringify({ cmd: 'fs_roots_list', args: {} }),
			});
			if (res.ok) {
				const json = await res.json();
				if (json.ok !== false) {
					try {
						sessionStorage.setItem('ikenga_auth_token', newToken.trim());
					} catch {
						// Ignore
					}
					if (typeof window !== 'undefined') {
						window.location.reload();
					}
					return true;
				}
			}
			set({ errorMsg: 'Invalid token — daemon rejected authorization.' });
			return false;
		} catch (err) {
			set({ errorMsg: `Connection failed: ${String(err)}` });
			return false;
		}
	},
	signIn: async (username: string, password: string) => {
		if (!username.trim() || !password) {
			set({ errorMsg: 'Enter your username and password.' });
			return false;
		}
		const result = await signInWithPassword(username.trim(), password);
		if (!result.ok) {
			set({ errorMsg: result.message });
			return false;
		}
		if (typeof window !== 'undefined') {
			window.location.reload();
		}
		return true;
	},
}));
