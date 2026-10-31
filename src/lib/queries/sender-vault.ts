// WP-K — the sender vault's status (names, lengths, times; never values).

import { useQuery } from '@tanstack/react-query';

import { queryKeys } from '@/lib/query-keys';
import {
	fetchSenderVaultStatus,
	mayManageSenderVault,
	type VaultStatus,
} from '@/lib/transport/sender-vault';

/** Resolves to `null` when the sender refuses or is off (the section then says why). */
export function useSenderVaultStatus() {
	return useQuery({
		queryKey: queryKeys.senderVault.status(),
		enabled: mayManageSenderVault(),
		staleTime: 15_000,
		refetchOnWindowFocus: false,
		retry: false,
		queryFn: async (): Promise<
			{ ok: true; status: VaultStatus } | { ok: false; code: string; message: string }
		> => {
			const r = await fetchSenderVaultStatus();
			return r.ok ? { ok: true, status: r.data } : { ok: false, code: r.code, message: r.message };
		},
	});
}
