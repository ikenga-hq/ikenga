import { create } from 'zustand';

interface ReauthStore {
	isOpen: boolean;
	showReauth: () => void;
	hideReauth: () => void;
	tokenInput: string;
	setTokenInput: (val: string) => void;
	errorMsg: string | null;
	setErrorMsg: (msg: string | null) => void;
	reconnect: (token: string) => Promise<boolean>;
}

export const useReauthStore = create<ReauthStore>((set) => ({
	isOpen: false,
	tokenInput: '',
	errorMsg: null,
	showReauth: () => set({ isOpen: true, errorMsg: null }),
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
