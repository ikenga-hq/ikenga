import {
	listen,
	settingsOpenFile as openSettingsFileCommand,
	settingsReadFile as readSettingsFileCommand,
	settingsWriteField as writeSettingsFieldCommand,
	type UnlistenFn,
} from '@/lib/tauri-cmd';
import type {
	SettingsChangeEvent,
	SettingsFileResult,
	SettingsScope,
	SettingsWriteOptions,
} from './types';

export {
	PERSONAL_ONLY_FIELDS,
	PROJECT_ONLY_FIELDS,
	SETTINGS_DEFAULTS,
} from './types';
export type {
	SettingsChangeEvent,
	SettingsDocument,
	SettingsField,
	SettingsFileResult,
	SettingsScope,
	SettingsWriteEntry,
	SettingsWriteOptions,
} from './types';

export async function readSettingsFile(options: {
	scope?: SettingsScope;
	projectId?: string | null;
} = {}): Promise<SettingsFileResult> {
	return readSettingsFileCommand(options.scope ?? 'project', options.projectId ?? null);
}

export async function writeSettingsField(options: SettingsWriteOptions): Promise<SettingsFileResult> {
	return writeSettingsFieldCommand(options);
}

export async function writeSettingsFields(
	fields: SettingsWriteOptions[],
	projectId?: string | null,
): Promise<SettingsFileResult | null> {
	let result: SettingsFileResult | null = null;
	for (const field of fields) {
		const entry: SettingsWriteOptions =
			field.scope === 'project' && field.projectId == null && projectId != null
				? Object.assign({}, field, { projectId })
				: field;
		result = await writeSettingsField(entry);
	}
	return result;
}

export async function openSettingsFile(
	scope: SettingsScope,
	projectId?: string | null,
): Promise<string> {
	return openSettingsFileCommand(scope, projectId ?? null);
}

export function watchSettings(onChange: () => void | Promise<void>): Promise<UnlistenFn> {
	let timer: ReturnType<typeof setTimeout> | null = null;
	let active = true;
	let unlisten: UnlistenFn | null = null;
	return listen<SettingsChangeEvent>('settings://changed', () => {
		if (!active) return;
		if (timer) clearTimeout(timer);
		timer = setTimeout(() => {
			timer = null;
			if (!active) return;
			void Promise.resolve(onChange()).catch(() => {});
		}, 50);
	}).then((stop) => {
		unlisten = stop;
		return () => {
			if (!active) return;
			active = false;
			if (timer) {
				clearTimeout(timer);
				timer = null;
			}
			unlisten?.();
		};
	});
}
