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

import { CUSTOM_SHELL_KEY, parseCustomShellProfiles, useCustomShellProfiles } from './shell-profiles';

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
