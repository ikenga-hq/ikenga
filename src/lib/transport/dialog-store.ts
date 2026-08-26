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

export const useDialogStore = create<DialogStore>((set, get) => ({
	activeRequest: null,
	requestOpen: (options) => {
		return new Promise<string | string[] | null>((resolve) => {
			set({
				activeRequest: {
					id: Math.random().toString(36).slice(2),
					type: 'open',
					options,
					resolve: resolve as (v: string | string[] | boolean | null) => void,
				},
			});
		});
	},
	requestSave: (options) => {
		return new Promise<string | null>((resolve) => {
			set({
				activeRequest: {
					id: Math.random().toString(36).slice(2),
					type: 'save',
					options,
					resolve: resolve as (v: string | string[] | boolean | null) => void,
				},
			});
		});
	},
	closeDialog: (result) => {
		const req = get().activeRequest;
		if (req) {
			req.resolve(result);
			set({ activeRequest: null });
		}
	},
}));
