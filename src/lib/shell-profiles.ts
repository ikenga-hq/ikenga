import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { isWindows } from '@/lib/platform';
import { settingsGet, settingsSet, terminalDetectShells, type ShellProfile } from '@/lib/tauri-cmd';

export const DEFAULT_SHELL_KEY = 'terminal.default_shell_id';
export const CUSTOM_SHELL_KEY = 'terminal.custom_shell_profiles';
export const AGENT_ENV_KEY = 'terminal.agent_env_kind'; // 'native' | 'wsl'
export const AGENT_WSL_DISTRO_KEY = 'terminal.agent_wsl_distro';
export const RESUME_TERMINALS_KEY = 'terminal.resume_on_start';

export function getFallbackProfile(): ShellProfile {
	if (isWindows) {
		return {
			id: 'powershell',
			label: 'Windows PowerShell',
			icon: 'powershell',
			cmd: ['powershell.exe', '-NoLogo'],
			isDefault: true,
			kind: 'powershell',
			distro: null,
		};
	}
	return {
		id: 'bash',
		label: 'bash',
		icon: 'bash',
		cmd: ['bash', '-l'],
		isDefault: true,
		kind: 'bash',
		distro: null,
	};
}

/**
 * Parse the stored custom-profile list. `null` (never saved) is a confirmed
 * empty list; anything unreadable throws, because the caller must not treat
 * "couldn't read" as "there are none" — the next add/remove would overwrite
 * the saved list with one that's missing every saved profile.
 */
export function parseCustomShellProfiles(raw: string | null): ShellProfile[] {
	if (raw === null || raw === '') return [];
	let parsed: unknown;
	try {
		parsed = JSON.parse(raw);
	} catch (err) {
		throw new CorruptCustomShellsError(`saved custom shell profiles are not valid JSON (${String(err)})`, raw);
	}
	if (!Array.isArray(parsed)) {
		throw new CorruptCustomShellsError('saved custom shell profiles are not a list', raw);
	}
	return parsed as ShellProfile[];
}

/**
 * The saved custom-shells value was read but is corrupt (not a transient read
 * failure). Carries the raw value so a reset can back it up first (D-13).
 */
export class CorruptCustomShellsError extends Error {
	constructor(
		message: string,
		readonly raw: string
	) {
		super(message);
		this.name = 'CorruptCustomShellsError';
	}
}

/** Side key a corrupt custom-shells value is copied to before a reset. */
export function customShellsBackupKey(now: number = Date.now()): string {
	return `${CUSTOM_SHELL_KEY}.corrupt-${now}`;
}

/**
 * Reset a corrupt custom-shells list: copy the raw value to a side key, and
 * only once that write succeeded, clear the list. Returns the backup key.
 * Throws (with nothing cleared) if the backup can't be written.
 */
export async function resetCorruptCustomShells(raw: string, now: number = Date.now()): Promise<string> {
	const backupKey = customShellsBackupKey(now);
	await settingsSet(backupKey, raw);
	await settingsSet(CUSTOM_SHELL_KEY, '[]');
	return backupKey;
}

export function useCustomShellProfiles() {
	const queryClient = useQueryClient();

	// A failed read surfaces as a query error — never as `[]`.
	const customQuery = useQuery<ShellProfile[]>({
		queryKey: ['settings', CUSTOM_SHELL_KEY],
		queryFn: async () => parseCustomShellProfiles(await settingsGet(CUSTOM_SHELL_KEY)),
		staleTime: 60_000,
	});

	const saveMutation = useMutation({
		mutationFn: async (profiles: ShellProfile[]) => {
			await settingsSet(CUSTOM_SHELL_KEY, JSON.stringify(profiles));
		},
		onSuccess: () => {
			void queryClient.invalidateQueries({ queryKey: ['settings', CUSTOM_SHELL_KEY] });
			void queryClient.invalidateQueries({ queryKey: ['terminal', 'shells'] });
		},
	});

	// Writes rewrite the whole list, so they are only safe once the current
	// list has been read successfully. Until then add/remove refuse.
	const canEdit = customQuery.isSuccess;

	// Corrupt (not merely unreadable right now): offer a reset that backs the
	// bad value up to a side key first, then clears the list (D-13).
	const corrupt = customQuery.error instanceof CorruptCustomShellsError ? customQuery.error : null;
	const resetMutation = useMutation({
		mutationFn: async (raw: string) => resetCorruptCustomShells(raw),
		onSuccess: () => {
			void queryClient.invalidateQueries({ queryKey: ['settings', CUSTOM_SHELL_KEY] });
			void queryClient.invalidateQueries({ queryKey: ['terminal', 'shells'] });
		},
	});
	const resetCorrupt = async (): Promise<string | null> => {
		if (!corrupt) return null;
		return resetMutation.mutateAsync(corrupt.raw);
	};

	const addCustomProfile = (profile: Omit<ShellProfile, 'id' | 'isDefault'>): boolean => {
		if (!customQuery.isSuccess) return false;
		const newProfile: ShellProfile = {
			...profile,
			id: `custom-${Date.now()}`,
			isDefault: false,
		};
		saveMutation.mutate([...customQuery.data, newProfile]);
		return true;
	};

	const removeCustomProfile = (id: string): boolean => {
		if (!customQuery.isSuccess) return false;
		saveMutation.mutate(customQuery.data.filter((p) => p.id !== id));
		return true;
	};

	return {
		customProfiles: customQuery.data ?? [],
		addCustomProfile,
		removeCustomProfile,
		canEdit,
		/** True when the saved value is corrupt and `resetCorrupt` is offered. */
		isCorrupt: corrupt !== null,
		resetCorrupt,
		/** Side key the corrupt value was copied to by the last reset. */
		resetBackupKey: resetMutation.data ?? null,
		resetError: resetMutation.error,
		isResetting: resetMutation.isPending,
		isLoading: customQuery.isLoading,
		error: customQuery.error,
		refetch: customQuery.refetch,
	};
}

export function useShellProfiles() {
	const { customProfiles } = useCustomShellProfiles();

	return useQuery<ShellProfile[]>({
		queryKey: ['terminal', 'shells', customProfiles.map((c) => c.id).join(',')],
		queryFn: async () => {
			let detected: ShellProfile[] = [];
			try {
				const res = await terminalDetectShells();
				if (res && res.length > 0) {
					detected = res;
				}
			} catch (err) {
				console.warn('[shell-profiles] Failed to detect shells from backend:', err);
			}
			if (detected.length === 0) {
				detected = [getFallbackProfile()];
			}
			return [...detected, ...customProfiles];
		},
		staleTime: 60_000,
	});
}

function reasonOf(err: unknown): string {
	return err instanceof Error ? err.message : String(err);
}

/**
 * Why the default shell couldn't be resolved as saved, or `null` when it
 * was. Exported for tests. A failed read is "couldn't tell", not "no saved
 * default" — callers must say a fallback is a fallback, not open it silently.
 */
export function defaultShellReadError(opts: {
	settingError: unknown;
	customError: unknown;
	savedId: string | null | undefined;
	resolved: boolean;
	fallbackLabel: string;
}): string | null {
	const { settingError, customError, savedId, resolved, fallbackLabel } = opts;
	if (settingError) {
		return `Couldn't read your default shell setting (${reasonOf(settingError)}). Using ${fallbackLabel} for now.`;
	}
	if (customError) {
		// Saved default is a custom shell we couldn't load → wrong shell would open.
		if (savedId && !resolved) {
			return `Couldn't read your saved custom shells (${reasonOf(customError)}), so your default shell isn't available. Using ${fallbackLabel} for now.`;
		}
		return `Couldn't read your saved custom shells (${reasonOf(customError)}). They're missing from this list until it loads.`;
	}
	return null;
}

export function useDefaultShellProfile() {
	const queryClient = useQueryClient();
	const { data: profiles = [getFallbackProfile()], isLoading: isProfilesLoading } =
		useShellProfiles();
	const { error: customError, refetch: refetchCustom } = useCustomShellProfiles();

	// A failed read surfaces as a query error — never as "no saved default".
	const settingQuery = useQuery<string | null>({
		queryKey: ['settings', DEFAULT_SHELL_KEY],
		queryFn: () => settingsGet(DEFAULT_SHELL_KEY),
		staleTime: 60_000,
	});

	const mutation = useMutation({
		mutationFn: async (profileId: string) => {
			await settingsSet(DEFAULT_SHELL_KEY, profileId);
		},
		onSuccess: () => {
			void queryClient.invalidateQueries({ queryKey: ['settings', DEFAULT_SHELL_KEY] });
		},
	});

	const savedId = settingQuery.data;
	const saved = savedId ? profiles.find((p) => p.id === savedId) : undefined;
	const selected =
		saved ?? profiles.find((p) => p.isDefault) ?? profiles[0] ?? getFallbackProfile();

	const readError = defaultShellReadError({
		settingError: settingQuery.error,
		customError,
		savedId,
		resolved: saved !== undefined,
		fallbackLabel: selected.label,
	});

	return {
		profiles,
		selectedProfile: selected,
		setDefaultProfileId: mutation.mutate,
		isLoading: isProfilesLoading || settingQuery.isLoading,
		/** Set when the saved default or custom shells couldn't be read; the
		 *  selection above is then a fallback and the UI must say so. */
		readError,
		retryRead: () => {
			void settingQuery.refetch();
			void refetchCustom();
		},
	};
}
