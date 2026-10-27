import { queryOptions, useMutation, useQueryClient } from '@tanstack/react-query';

import { queryKeys } from '@/lib/query-keys';
import {
	type SecretsLockState,
	secretsDelete,
	secretsDeleteScoped,
	secretsListKeys,
	secretsListKeysScoped,
	secretsLock,
	secretsQuarantined,
	type QuarantinedSecret,
	secretsLockState,
	secretsSet,
	secretsSetPassphrase,
	secretsSetScoped,
	secretsUnlock,
	secretsVaultStatus,
	type VaultScope,
	type VaultStatus,
} from '@/lib/tauri-cmd';

export type { SecretsLockState, VaultStatus };

export function vaultStatusQueryOptions() {
	return queryOptions({
		queryKey: queryKeys.secrets.vaultStatus(),
		queryFn: () => secretsVaultStatus(),
		staleTime: 30_000,
	});
}

export function secretsLockStateQueryOptions() {
	return queryOptions({
		queryKey: queryKeys.secrets.lockState(),
		queryFn: () => secretsLockState(),
		staleTime: 5_000,
	});
}

export function useSetSecretsPassphrase() {
	const qc = useQueryClient();
	return useMutation<
		SecretsLockState,
		Error,
		{ passphrase: string; currentPassphrase?: string | null }
	>({
		mutationFn: ({ passphrase, currentPassphrase }) =>
			secretsSetPassphrase(passphrase, currentPassphrase),
		onSuccess: (state) => {
			qc.setQueryData(queryKeys.secrets.lockState(), state);
			qc.invalidateQueries({ queryKey: queryKeys.secrets.all });
		},
	});
}

export function useUnlockSecrets() {
	const qc = useQueryClient();
	return useMutation<SecretsLockState, Error, string>({
		mutationFn: secretsUnlock,
		onSuccess: (state) => {
			qc.setQueryData(queryKeys.secrets.lockState(), state);
			qc.invalidateQueries({ queryKey: queryKeys.secrets.all });
		},
	});
}

export function useLockSecrets() {
	const qc = useQueryClient();
	return useMutation<SecretsLockState, Error, void>({
		mutationFn: () => secretsLock(),
		onSuccess: (state) => {
			qc.setQueryData(queryKeys.secrets.lockState(), state);
			qc.invalidateQueries({ queryKey: queryKeys.secrets.all });
		},
	});
}

export const vaultLockStateQueryOptions = secretsLockStateQueryOptions;
export const useSetSecretPassphrase = useSetSecretsPassphrase;
export const useUnlockSecret = useUnlockSecrets;
export const useLockSecret = useLockSecrets;

export function vaultKeysQueryOptions() {
	return queryOptions({
		queryKey: queryKeys.secrets.keys(),
		queryFn: () => secretsListKeys(),
		staleTime: 5_000,
	});
}

export function useSetSecret() {
	const qc = useQueryClient();
	return useMutation<void, Error, { key: string; value: string }>({
		mutationFn: ({ key, value }) => secretsSet(key, value),
		onSuccess: () => {
			qc.invalidateQueries({ queryKey: queryKeys.secrets.all });
		},
	});
}

export function useDeleteSecret() {
	const qc = useQueryClient();
	return useMutation<void, Error, string>({
		mutationFn: (key) => secretsDelete(key),
		onSuccess: () => {
			qc.invalidateQueries({ queryKey: queryKeys.secrets.all });
		},
	});
}

// ─── Phase 7 — scoped variants ────────────────────────────────────────────

function scopeKey(scope: VaultScope): string {
	return scope.kind === 'workspace' ? 'workspace' : `${scope.kind}:${scope.id}`;
}

export function vaultKeysScopedQueryOptions(scope: VaultScope) {
	return queryOptions({
		queryKey: [...queryKeys.secrets.all, 'scoped', scopeKey(scope)] as const,
		queryFn: () => secretsListKeysScoped(scope),
		staleTime: 5_000,
	});
}

export function useSetScopedSecret() {
	const qc = useQueryClient();
	return useMutation<void, Error, { scope: VaultScope; key: string; value: string }>({
		mutationFn: ({ scope, key, value }) => secretsSetScoped(scope, key, value),
		onSuccess: () => {
			qc.invalidateQueries({ queryKey: queryKeys.secrets.all });
		},
	});
}

export function useDeleteScopedSecret() {
	const qc = useQueryClient();
	return useMutation<void, Error, { scope: VaultScope; key: string }>({
		mutationFn: ({ scope, key }) => secretsDeleteScoped(scope, key),
		onSuccess: () => {
			qc.invalidateQueries({ queryKey: queryKeys.secrets.all });
		},
	});
}

// ─── Needs re-entry (partially failed keychain migration) ─────────────────

export type { QuarantinedSecret };

export function quarantinedSecretsQueryOptions() {
	return queryOptions({
		queryKey: [...queryKeys.secrets.all, 'quarantined'] as const,
		queryFn: () => secretsQuarantined(),
		staleTime: 5_000,
	});
}

/** Enter the value again. A legacy bare name goes through the unscoped
 *  command, everything else through its own scope. */
export function useSetQuarantinedSecret() {
	const qc = useQueryClient();
	return useMutation<void, Error, { entry: QuarantinedSecret; value: string }>({
		mutationFn: ({ entry, value }) =>
			entry.scope ? secretsSetScoped(entry.scope, entry.key, value) : secretsSet(entry.name, value),
		onSuccess: () => {
			qc.invalidateQueries({ queryKey: queryKeys.secrets.all });
		},
	});
}

export function useDeleteQuarantinedSecret() {
	const qc = useQueryClient();
	return useMutation<void, Error, QuarantinedSecret>({
		mutationFn: (entry) =>
			entry.scope ? secretsDeleteScoped(entry.scope, entry.key) : secretsDelete(entry.name),
		onSuccess: () => {
			qc.invalidateQueries({ queryKey: queryKeys.secrets.all });
		},
	});
}
