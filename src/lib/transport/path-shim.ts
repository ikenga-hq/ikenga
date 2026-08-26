import { isTauri } from './index';

export async function homeDir(): Promise<string> {
	if (isTauri()) {
		const { homeDir } = await import('@tauri-apps/api/path');
		return homeDir();
	}
	try {
		const { getTransport } = await import('./index');
		const res = await getTransport().invoke<string>('fs_home');
		if (res) return res;
	} catch {
		// Fall through
	}
	return '~';
}

export async function appDataDir(): Promise<string> {
	if (isTauri()) {
		const { appDataDir } = await import('@tauri-apps/api/path');
		return appDataDir();
	}
	return '~/.local/share/app.ikenga';
}

export async function join(...paths: string[]): Promise<string> {
	if (isTauri()) {
		const { join } = await import('@tauri-apps/api/path');
		return join(...paths);
	}
	return paths
		.filter(Boolean)
		.join('/')
		.replace(/\/+/g, '/');
}
