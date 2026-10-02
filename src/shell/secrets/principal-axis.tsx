// D-03 Secrets — the principal axis on remote (WP-76, over remote-access
// WP-21's per-principal store; G-ACCESS §10.2).
//
// On the desktop the vault is the OS keychain and there is one principal, so
// nothing changes. In a browser the page says *whose* vault it is:
//
// - **T1, your own workspace** (`secrets_vault_status.mode === 'principal'`):
//   your own store under the server's `<data>/secrets/`, sealed under a key
//   the server holds for you (DEC-R18-1 — no passphrase, nothing to unlock),
//   layered over the operator default (`IKENGA_SECRET_*`, read-only). All
//   three scopes (Workspace / Project / Pkg) are yours.
// - **T0 daemon in a browser** (`mode === 'env'`): only the operator default
//   — one flat, read-only namespace; Project and Pkg scopes are refused by
//   the daemon, so their tabs are disabled with that reason.
// - **A shared project** (share mode, G-ACCESS §4.5): secrets are never
//   reachable through a share (owner-class arms, §4.1 "never"), so the page
//   shows why instead of a vault.

import { KeyRound, Server, ShieldAlert, UserRound } from 'lucide-react';
import type { ReactNode } from 'react';

import { cn } from '@/components/ui/utils';
import type { ShareSelection } from '@/lib/transport';

export type VaultAxis = 'desktop' | 'principal' | 'operator' | 'shared';
export type ScopeKind = 'workspace' | 'project' | 'pkg';

/** Which vault this page shows. */
export function vaultAxis(opts: {
	remote: boolean;
	mode: string | null | undefined;
	share: ShareSelection | null;
}): VaultAxis {
	if (!opts.remote) return 'desktop';
	if (opts.share) return 'shared';
	return opts.mode === 'principal' ? 'principal' : 'operator';
}

export const OPERATOR_SCOPE_REASON =
	"The server's operator default is one flat namespace. Project and pkg secrets need your own store — an Ikenga server with accounts.";
export const SHARED_REASON =
	"Secrets stay with the project's Owner. A shared project shares files, sessions and dispatch — never credentials.";

/** Why a scope tab is disabled on this axis, or `null` when it is usable. */
export function scopeDisabledReason(axis: VaultAxis, scope: ScopeKind): string | null {
	if (axis === 'shared') return SHARED_REASON;
	if (axis === 'operator' && scope !== 'workspace') return OPERATOR_SCOPE_REASON;
	return null;
}

/** Which layer a listed key comes from (review WP76-R3). On a principal
 *  store the Workspace list merges the operator default's env keys in
 *  (`DaemonSecrets::list_keys_scoped`), so a row is:
 *  - `own`: only in your store (edit, delete);
 *  - `override`: in your store over an operator default (edit, "Remove your
 *    override" — the default shows through again);
 *  - `default`: only the operator default (read-only; "Override" writes a
 *    value of your own into your store).
 *  `indexNames` is `secrets_index_names` (env names bare, store names
 *  `workspace::KEY`); while it is unknown every principal Workspace row is
 *  treated as `default` (read-only) rather than presented as yours. */
export type SecretLayer = 'own' | 'override' | 'default';

export function secretLayer(
	axis: VaultAxis,
	scope: ScopeKind,
	key: string,
	indexNames: readonly string[] | undefined
): SecretLayer {
	if (axis === 'operator') return 'default';
	if (axis !== 'principal' || scope !== 'workspace') return 'own';
	if (!indexNames) return 'default';
	const inStore = indexNames.includes(`workspace::${key}`);
	if (!inStore) return 'default';
	return indexNames.includes(key) ? 'override' : 'own';
}

/** Whether the lock / passphrase controls apply: only the desktop keychain
 *  has a passphrase layer; a principal store is sealed server-side. */
export function hasPassphraseLayer(axis: VaultAxis): boolean {
	return axis === 'desktop';
}

export function PrincipalAxis({
	axis,
	username,
	share,
}: {
	axis: VaultAxis;
	username: string | null;
	share: ShareSelection | null;
}) {
	if (axis === 'desktop') return null;
	if (axis === 'shared') {
		return (
			<div
				data-state="secrets-shared"
				className="flex items-start gap-3 rounded-md border border-border bg-card px-4 py-3 text-xs"
			>
				<ShieldAlert className="mt-0.5 h-4 w-4 shrink-0 text-[var(--danger)]" />
				<div className="space-y-1">
					<p className="m-0 font-medium text-foreground">
						{share?.projectName ?? 'This project'} is shared with you.
					</p>
					<p className="m-0 text-muted-foreground">{SHARED_REASON}</p>
				</div>
			</div>
		);
	}
	const rows: Array<{ id: string; icon: ReactNode; title: string; sub: string; active: boolean }> =
		[
			{
				id: 'principal',
				icon: <UserRound className="h-3.5 w-3.5" />,
				title: username ? `${username}'s store` : 'Your store',
				sub:
					axis === 'principal'
						? 'Yours alone, on this server. Sealed under a key the server holds for you — there is no passphrase to unlock. Overrides the operator default key by key.'
						: 'Needs an Ikenga server with accounts (T1).',
				active: axis === 'principal',
			},
			{
				id: 'operator',
				icon: <Server className="h-3.5 w-3.5" />,
				title: 'Operator default',
				sub: 'IKENGA_SECRET_* set on the host by whoever runs the server. Read-only from here.',
				active: true,
			},
		];
	return (
		<div
			data-state={axis === 'principal' ? 'secrets-principal' : 'secrets-operator'}
			className="overflow-hidden rounded-md border border-border bg-card text-xs"
		>
			<div className="flex items-center gap-2 border-b border-border px-3 py-2">
				<KeyRound className="h-3.5 w-3.5 text-muted-foreground" />
				<span className="font-medium">Whose secrets</span>
				<span className="ml-auto font-mono text-[11px] text-muted-foreground">
					{axis === 'principal' ? 'your store over the operator default' : 'operator default only'}
				</span>
			</div>
			<ul className="m-0 list-none divide-y divide-border p-0">
				{rows.map((r) => (
					<li
						key={r.id}
						data-layer={r.id}
						data-active={r.active}
						className={cn('flex items-start gap-2 px-3 py-2', !r.active && 'opacity-60')}
					>
						<span className="mt-0.5 text-muted-foreground">{r.icon}</span>
						<span>
							<span className="block font-medium text-foreground">{r.title}</span>
							<span className="block text-muted-foreground">{r.sub}</span>
						</span>
					</li>
				))}
			</ul>
		</div>
	);
}
