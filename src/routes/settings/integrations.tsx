import { useQuery } from '@tanstack/react-query';
import { createFileRoute } from '@tanstack/react-router';
import { writeClipboardText } from '@/lib/transport';
import { CheckCircle2, Copy, KeyRound, XCircle } from 'lucide-react';
import { useState } from 'react';

import { FeedbackState } from '@/components/ui/feedback-state';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { StatusChip } from '@/components/ui/status-chip';
import { vaultKeysQueryOptions, vaultStatusQueryOptions } from '@/lib/queries/secrets';
import { iykeMcpInfo } from '@/lib/tauri-cmd';

import { ApiKeysSectionBody } from './-components/api-keys';
import { ClaudeSummarySectionBody } from './-components/claude-summary';
import { ConnectorCardsSection } from './-components/connector-cards';
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

// Iyke MCP section — surfaces the absolute path of the bundled MCP server
// binary so external clients (Claude Desktop, Cursor) can spawn it directly.
// The path is stable for a given install; on shell upgrades the resource
// dir typically stays the same on Linux/macOS, so configs remain valid.
function IykeMcpSection() {
	const info = useQuery({
		queryKey: ['iyke-mcp-info'],
		queryFn: iykeMcpInfo,
		staleTime: 30_000,
	});
	const [copied, setCopied] = useState<null | 'path' | 'json'>(null);

	if (info.isLoading) {
		return (
			<FeedbackState variant="loading" body="Resolving binary path…" className="min-h-0 py-4" />
		);
	}

	const data = info.data;
	if (!data || !data.path) {
		return (
			<FeedbackState
				variant="error"
				body="Could not resolve resource directory. Check that the app is running from a normal install."
				className="min-h-0 py-4"
			/>
		);
	}

	const configJson = JSON.stringify(
		{
			mcpServers: {
				iyke: {
					command: data.path,
					args: [],
				},
			},
		},
		null,
		2
	);

	async function copy(kind: 'path' | 'json', text: string) {
		try {
			await writeClipboardText(text);
			setCopied(kind);
			setTimeout(() => setCopied((c) => (c === kind ? null : c)), 1500);
		} catch (e) {
			console.error('[iyke-mcp] clipboard write failed', e);
		}
	}

	return (
		<>
			<SettingRow
				label="Status"
				desc="Bundled with the shell. While Ikenga is running, any MCP client configured against the binary path below can drive the desktop."
			>
				<StatusChip
					tone={data.present ? 'live' : 'warn'}
					icon={data.present ? CheckCircle2 : XCircle}
				>
					{data.present
						? `Bundled (${data.source})`
						: 'Build pending — run `bun run iyke:mcp:build`'}
				</StatusChip>
			</SettingRow>

			<SettingRow label="Binary path" desc="Absolute path on disk. Stable across shell relaunches.">
				<div className="flex w-72 items-center gap-1.5">
					<Input
						type="text"
						value={data.path}
						readOnly
						className="h-8 flex-1 font-mono text-[10px]"
					/>
					<button
						type="button"
						className="grid h-8 w-8 place-items-center rounded-md border border-border-soft text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground"
						onClick={() => void copy('path', data.path)}
						aria-label="Copy binary path"
						title={copied === 'path' ? 'Copied!' : 'Copy path'}
					>
						<Copy className="h-3.5 w-3.5" />
					</button>
				</div>
			</SettingRow>

			<div className="border-t border-border-soft px-4 py-3">
				<div className="mb-2 flex items-center justify-between">
					<div>
						<div className="text-[11px] font-semibold text-foreground">
							Claude Desktop / Cursor config
						</div>
						<div className="text-[11px] text-muted-foreground">
							Paste into <code className="font-mono">claude_desktop_config.json</code> (Claude
							Desktop) or your MCP client's config.
						</div>
					</div>
					<button
						type="button"
						className="inline-flex items-center gap-1.5 rounded-md border border-border-soft px-2 py-1 text-[11px] text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground"
						onClick={() => void copy('json', configJson)}
					>
						<Copy className="h-3 w-3" />
						{copied === 'json' ? 'Copied!' : 'Copy JSON'}
					</button>
				</div>
				<pre className="overflow-x-auto rounded border border-border-soft bg-muted/40 p-2 font-mono text-[10px] leading-relaxed">
					{configJson}
				</pre>
			</div>
		</>
	);
}

export const Route = createFileRoute('/settings/integrations')({
	component: IntegrationsPage,
});
