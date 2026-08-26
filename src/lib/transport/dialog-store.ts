import { create } from 'zustand';

export interface OpenDialogOptions {
	title?: string;
	defaultPath?: string;
	directory?: boolean;
	multiple?: boolean;
	filters?: Array<{ name: string; extensions: string[] }>;
}

export interface DialogRequest {
	id: string;
	type: 'open' | 'save' | 'confirm' | 'message';
	options: OpenDialogOptions;
	resolve: (value: string | string[] | boolean | null) => void;
}

interface DialogStore {
	activeRequest: DialogRequest | null;
	requestOpen: (options: OpenDialogOptions) => Promise<string | string[] | null>;
	requestSave: (options: OpenDialogOptions) => Promise<string | null>;
	closeDialog: (result: string | string[] | boolean | null) => void;
}

export const useDialogStore = create<DialogStore>((set, get) => {
	/**
	 * Install a request, settling any it displaces.
	 *
	 * Only one picker can be on screen, so a second request necessarily
	 * replaces the first. Replacing it without resolving leaves the original
	 * `await open(...)` pending forever — the caller never returns, and there
	 * is no longer any UI that could settle it. Resolving as `null` is the
	 * same answer the user would give by dismissing it.
	 */
	const enqueue = <T>(type: DialogRequest['type'], options: OpenDialogOptions): Promise<T> =>
		new Promise<T>((resolve) => {
			const displaced = get().activeRequest;
			if (displaced) displaced.resolve(null);
			set({
				activeRequest: {
					id: Math.random().toString(36).slice(2),
					type,
					options,
					resolve: resolve as (v: string | string[] | boolean | null) => void,
				},
			});
		});

	return {
		activeRequest: null,
		requestOpen: (options) => enqueue<string | string[] | null>('open', options),
		requestSave: (options) => enqueue<string | null>('save', options),
		closeDialog: (result) => {
			const req = get().activeRequest;
			if (req) {
				req.resolve(result);
				set({ activeRequest: null });
			}
		},
	};
});
