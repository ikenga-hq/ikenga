import { create } from 'zustand';

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
	showReauth: (reason?: ReauthReason) => void;
	hideReauth: () => void;
	tokenInput: string;
	setTokenInput: (val: string) => void;
	errorMsg: string | null;
	setErrorMsg: (msg: string | null) => void;
	reconnect: (token: string) => Promise<boolean>;
}

export const useReauthStore = create<ReauthStore>((set) => ({
	isOpen: false,
	reason: 'expired',
	tokenInput: '',
	errorMsg: null,
	showReauth: (reason = 'expired') => set({ isOpen: true, reason, errorMsg: null }),
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
}));
