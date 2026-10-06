import { useQuery } from '@tanstack/react-query';
import { createFileRoute } from '@tanstack/react-router';
import { CheckCircle2, KeyRound, XCircle } from 'lucide-react';

import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { StatusChip } from '@/components/ui/status-chip';
import { vaultKeysQueryOptions, vaultStatusQueryOptions } from '@/lib/queries/secrets';

import { ApiKeysSectionBody } from './-components/api-keys';
import { ClaudeSummarySectionBody } from './-components/claude-summary';
import { ConnectorCardsSection } from './-components/connector-cards';
import { IykeMcpSection } from './-components/iyke-mcp';
import { SettingRow } from './-components/setting-row';

function IntegrationsPage() {
	const status = useQuery(vaultStatusQueryOptions());
	const keys = useQuery(vaultKeysQueryOptions());

	const supabaseUrl = import.meta.env?.VITE_SUPABASE_URL ?? '';
	const known = new Set(keys.data ?? []);
	const supabaseAnonPresent = known.has('VITE_SUPABASE_ANON_KEY');
	const vaultAvailable = status.data?.available === true;

	const connected =
		(vaultAvailable ? 1 : 0) + (supabaseUrl ? 1 : 0) + (supabaseAnonPresent ? 1 : 0);

	return (
		<div className="mx-auto w-full max-w-[720px] space-y-6 px-6 py-6">
			<header className="flex items-start justify-between gap-4">
				<div className="space-y-1">
					<h2
						className="text-2xl font-semibold tracking-tight"
						style={{ fontFamily: 'var(--font-display)' }}
					>
						Integrations
					</h2>
					<p className="max-w-prose text-sm leading-relaxed text-muted-foreground">
						External services this workspace talks to. Claude Code drives in-app sessions and skill
						discovery; Supabase backs the cross-pkg sync layer; the vault feeds every sidecar
						instead of <code>.env</code> files.
					</p>
				</div>
				<StatusChip tone={connected > 0 ? 'live' : 'muted'} dot>
					{connected} connected
				</StatusChip>
			</header>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Connectors
					</h3>
				</header>
				<ConnectorCardsSection />
			</section>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Claude Code
					</h3>
				</header>
				<ClaudeSummarySectionBody />
			</section>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Iyke MCP
					</h3>
				</header>
				<IykeMcpSection />
			</section>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Supabase
					</h3>
				</header>
				<div className="divide-y divide-border">
					<SettingRow
						label="Project URL"
						desc="Read from VITE_SUPABASE_URL at build time. Pkgs with the Supabase capability get the URL and key threaded through the AppBridge host context, resolved from the vault."
					>
						<Input
							type="text"
							value={supabaseUrl || '(not set)'}
							disabled
							className="h-8 w-72 font-mono text-xs"
						/>
					</SettingRow>
					<SettingRow
						label="Anon key"
						desc="Managed in the vault below as VITE_SUPABASE_ANON_KEY — same value, single source."
					>
						<StatusChip
							tone={supabaseAnonPresent ? 'live' : 'danger'}
							icon={supabaseAnonPresent ? CheckCircle2 : XCircle}
						>
							{supabaseAnonPresent ? 'In vault' : 'Not set'}
						</StatusChip>
					</SettingRow>
				</div>
			</section>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						API keys
					</h3>
				</header>
				<ApiKeysSectionBody />
			</section>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Bridge API keys
					</h3>
				</header>
				<div className="flex items-start gap-3 px-4 py-3 text-xs text-muted-foreground">
					<KeyRound className="mt-0.5 h-3.5 w-3.5 shrink-0" />
					<span>
						Per-key create / scope / revoke is not available yet: the local iyke bridge
						authenticates with a single bearer token generated fresh each launch and shown to
						trusted surfaces only. Per-key management arrives with the trust-gating phase.
					</span>
				</div>
				<div className="flex gap-2 border-t border-border px-4 py-3">
					<Button variant="outline" size="sm" disabled title="The bridge has one per-launch bearer token; per-key management does not exist yet">
						Create key
					</Button>
					<Button variant="outline" size="sm" disabled title="Scoping needs per-key storage, which does not exist yet">
						Scopes
					</Button>
					<Button variant="outline" size="sm" disabled title="Revocation needs per-key storage, which does not exist yet">
						Revoke
					</Button>
				</div>
			</section>
		</div>
	);
}

export const Route = createFileRoute('/settings/integrations')({
	component: IntegrationsPage,
});
