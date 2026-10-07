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
		throw new Error(`saved custom shell profiles are not valid JSON (${String(err)})`);
	}
	if (!Array.isArray(parsed)) {
		throw new Error('saved custom shell profiles are not a list');
	}
	return parsed as ShellProfile[];
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

export function useDefaultShellProfile() {
	const queryClient = useQueryClient();
	const { data: profiles = [getFallbackProfile()], isLoading: isProfilesLoading } =
		useShellProfiles();

	const settingQuery = useQuery<string | null>({
		queryKey: ['settings', DEFAULT_SHELL_KEY],
		queryFn: async () => {
			try {
				return await settingsGet(DEFAULT_SHELL_KEY);
			} catch {
				return null;
			}
		},
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
	const selected =
		profiles.find((p) => p.id === savedId) ??
		profiles.find((p) => p.isDefault) ??
		profiles[0] ??
		getFallbackProfile();

	return {
		profiles,
		selectedProfile: selected,
		setDefaultProfileId: mutation.mutate,
		isLoading: isProfilesLoading || settingQuery.isLoading,
	};
}
