import { isTauri } from './index';

/** `fs_home` is a round trip per call otherwise, and `$HOME` cannot change
 *  under a running daemon. */
let cachedHome: string | null = null;

export async function homeDir(): Promise<string> {
	if (isTauri()) {
		const { homeDir } = await import('@tauri-apps/api/path');
		return homeDir();
	}
	if (cachedHome) return cachedHome;
	try {
		const { getTransport } = await import('./index');
		const res = await getTransport().invoke<string>('fs_home');
		if (res) {
			cachedHome = res;
			return res;
		}
	} catch (e) {
		// Worth a warning: a silent `~` propagates into every path built from
		// it and surfaces later as an unrelated-looking file error.
		console.warn('[path-shim] fs_home failed; falling back to "~"', e);
	}
	return '~';
}

export async function appDataDir(): Promise<string> {
	if (isTauri()) {
		const { appDataDir } = await import('@tauri-apps/api/path');
		return appDataDir();
	}
	// Derived from the daemon's real `$HOME` rather than hardcoded with a
	// literal `~`: the result gets joined into paths and handed to `fs_read`,
	// which does no tilde expansion, so an unexpanded `~` fails as a
	// missing-file error that looks nothing like its actual cause.
	const home = await homeDir();
	return join(home, '.local', 'share', 'app.ikenga');
}

export async function join(...paths: string[]): Promise<string> {
	if (isTauri()) {
		const { join } = await import('@tauri-apps/api/path');
		return join(...paths);
	}
	return paths.filter(Boolean).join('/').replace(/\/+/g, '/');
}
