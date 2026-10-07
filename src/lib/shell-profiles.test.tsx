// Custom shell profiles — a failed read must never be treated as "no saved
// profiles". The add/remove writes rewrite the whole list, so a read failure
// followed by an add used to persist `[new]` and wipe every saved shell.

import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

const settingsGetMock = vi.fn();
const settingsSetMock = vi.fn().mockResolvedValue(undefined);

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	settingsGet: (...args: unknown[]) => settingsGetMock(...args),
	settingsSet: (...args: unknown[]) => settingsSetMock(...args),
	terminalDetectShells: vi.fn().mockResolvedValue([]),
}));

import {
	CUSTOM_SHELL_KEY,
	DEFAULT_SHELL_KEY,
	defaultShellReadError,
	CorruptCustomShellsError,
	parseCustomShellProfiles,
	resetCorruptCustomShells,
	useCustomShellProfiles,
	useDefaultShellProfile,
} from './shell-profiles';

const SAVED = [
	{ id: 'custom-1', label: 'MSYS2', icon: 'terminal', cmd: ['bash.exe'], isDefault: false, kind: 'custom', distro: null },
];

function wrapper() {
	const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return ({ children }: { children: ReactNode }) => (
		<QueryClientProvider client={client}>{children}</QueryClientProvider>
	);
}

const NEW_PROFILE = { label: 'Venv', icon: 'terminal', cmd: ['py'], kind: 'custom' as const, distro: null };

describe('parseCustomShellProfiles', () => {
	it('treats a never-saved value as a confirmed empty list', () => {
		expect(parseCustomShellProfiles(null)).toEqual([]);
	});

	it('throws on unreadable data instead of returning []', () => {
		expect(() => parseCustomShellProfiles('{not json')).toThrow(/not valid JSON/);
		expect(() => parseCustomShellProfiles('{"a":1}')).toThrow(/not a list/);
	});
});

describe('useCustomShellProfiles', () => {
	beforeEach(() => {
		settingsGetMock.mockReset();
		settingsSetMock.mockClear();
	});

	it('a failed read surfaces as an error and add does NOT persist a list missing saved profiles', async () => {
		settingsGetMock.mockRejectedValue(new Error('ipc timeout'));
		const { result } = renderHook(() => useCustomShellProfiles(), { wrapper: wrapper() });

		await waitFor(() => expect(result.current.error).toBeTruthy());
		expect(result.current.canEdit).toBe(false);

		let saved = true;
		act(() => {
			saved = result.current.addCustomProfile(NEW_PROFILE);
		});
		expect(saved).toBe(false);

		let removed = true;
		act(() => {
			removed = result.current.removeCustomProfile('custom-1');
		});
		expect(removed).toBe(false);
		expect(settingsSetMock).not.toHaveBeenCalled();
	});

	it('once the read succeeds, add appends to the saved list', async () => {
		settingsGetMock.mockResolvedValue(JSON.stringify(SAVED));
		const { result } = renderHook(() => useCustomShellProfiles(), { wrapper: wrapper() });

		await waitFor(() => expect(result.current.canEdit).toBe(true));
		act(() => {
			result.current.addCustomProfile(NEW_PROFILE);
		});

		await waitFor(() => expect(settingsSetMock).toHaveBeenCalledTimes(1));
		const [key, value] = settingsSetMock.mock.calls[0];
		expect(key).toBe(CUSTOM_SHELL_KEY);
		const written = JSON.parse(value as string) as { id: string }[];
		expect(written.map((p) => p.id)[0]).toBe('custom-1');
		expect(written).toHaveLength(2);
	});
});

describe('corrupt custom shells (D-13)', () => {
	beforeEach(() => {
		settingsGetMock.mockReset();
		settingsSetMock.mockReset();
		settingsSetMock.mockResolvedValue(undefined);
	});

	it('a parse failure is "corrupt" and carries the raw value', () => {
		let caught: unknown;
		try {
			parseCustomShellProfiles('{not json');
		} catch (err) {
			caught = err;
		}
		expect(caught).toBeInstanceOf(CorruptCustomShellsError);
		expect((caught as CorruptCustomShellsError).raw).toBe('{not json');
	});

	it('a transient read failure is not corrupt: no reset offered', async () => {
		settingsGetMock.mockRejectedValue(new Error('ipc timeout'));
		const { result } = renderHook(() => useCustomShellProfiles(), { wrapper: wrapper() });
		await waitFor(() => expect(result.current.error).toBeTruthy());
		expect(result.current.isCorrupt).toBe(false);
		await act(async () => {
			expect(await result.current.resetCorrupt()).toBeNull();
		});
		expect(settingsSetMock).not.toHaveBeenCalled();
	});

	it('reset backs the raw value up to a side key first, then clears the list and re-enables editing', async () => {
		let stored: string | null = '{not json';
		settingsGetMock.mockImplementation(async (key: string) => (key === CUSTOM_SHELL_KEY ? stored : null));
		settingsSetMock.mockImplementation(async (key: string, value: string) => {
			if (key === CUSTOM_SHELL_KEY) stored = value;
		});
		const { result } = renderHook(() => useCustomShellProfiles(), { wrapper: wrapper() });
		await waitFor(() => expect(result.current.isCorrupt).toBe(true));
		expect(result.current.canEdit).toBe(false);

		let backupKey: string | null = null;
		await act(async () => {
			backupKey = await result.current.resetCorrupt();
		});
		expect(String(backupKey).startsWith(`${CUSTOM_SHELL_KEY}.corrupt-`)).toBe(true);
		expect(settingsSetMock.mock.calls[0]).toEqual([backupKey, '{not json']);
		expect(settingsSetMock.mock.calls[1]).toEqual([CUSTOM_SHELL_KEY, '[]']);

		await waitFor(() => expect(result.current.canEdit).toBe(true));
		expect(result.current.isCorrupt).toBe(false);
		expect(result.current.resetBackupKey).toBe(backupKey);
	});

	it('does not clear the list when the backup write fails', async () => {
		settingsSetMock.mockRejectedValueOnce(new Error('disk full'));
		await expect(resetCorruptCustomShells('{bad', 42)).rejects.toThrow('disk full');
		expect(settingsSetMock).toHaveBeenCalledTimes(1);
		expect(settingsSetMock.mock.calls[0]).toEqual([`${CUSTOM_SHELL_KEY}.corrupt-42`, '{bad']);
	});
});

describe('defaultShellReadError', () => {
	const base = { settingError: null, customError: null, savedId: 'custom-1', resolved: true, fallbackLabel: 'pwsh' };

	it('is null when everything was read', () => {
		expect(defaultShellReadError(base)).toBeNull();
	});

	it('names a failed default-shell read and the fallback in use', () => {
		const msg = defaultShellReadError({ ...base, settingError: new Error('ipc timeout'), savedId: undefined, resolved: false });
		expect(msg).toMatch(/default shell setting \(ipc timeout\)/);
		expect(msg).toMatch(/Using pwsh/);
	});

	it('says the saved custom default is unavailable when custom shells could not be read', () => {
		const msg = defaultShellReadError({ ...base, customError: new Error('bad json'), resolved: false });
		expect(msg).toMatch(/default shell isn't available/);
		expect(msg).toMatch(/Using pwsh/);
	});
});

describe('useDefaultShellProfile', () => {
	beforeEach(() => {
		settingsGetMock.mockReset();
	});

	it('a failed custom-shells read with a custom saved default reports the fallback instead of hiding it', async () => {
		settingsGetMock.mockImplementation(async (key: string) => {
			if (key === CUSTOM_SHELL_KEY) throw new Error('ipc timeout');
			if (key === DEFAULT_SHELL_KEY) return 'custom-1';
			return null;
		});
		const { result } = renderHook(() => useDefaultShellProfile(), { wrapper: wrapper() });
		await waitFor(() => expect(result.current.readError).toMatch(/custom shells \(ipc timeout\)/));
		expect(result.current.selectedProfile.id).not.toBe('custom-1');
		expect(result.current.readError).toMatch(/default shell isn't available/);
	});

	it('a failed default-shell read is an error, not "no saved default"', async () => {
		settingsGetMock.mockImplementation(async (key: string) => {
			if (key === DEFAULT_SHELL_KEY) throw new Error('db locked');
			return null;
		});
		const { result } = renderHook(() => useDefaultShellProfile(), { wrapper: wrapper() });
		await waitFor(() => expect(result.current.readError).toMatch(/db locked/));
	});

	it('a clean read reports no error and selects the saved custom default', async () => {
		settingsGetMock.mockImplementation(async (key: string) => {
			if (key === CUSTOM_SHELL_KEY) return JSON.stringify(SAVED);
			if (key === DEFAULT_SHELL_KEY) return 'custom-1';
			return null;
		});
		const { result } = renderHook(() => useDefaultShellProfile(), { wrapper: wrapper() });
		await waitFor(() => expect(result.current.selectedProfile.id).toBe('custom-1'));
		expect(result.current.readError).toBeNull();
	});
});
