// Iyke MCP card for Settings → Integrations (moved out of the route file so
// it can be tested on its own).

import { useQuery } from '@tanstack/react-query';
import { CheckCircle2, Copy, XCircle } from 'lucide-react';
import { useState } from 'react';

import { FeedbackState } from '@/components/ui/feedback-state';
import { Input } from '@/components/ui/input';
import { StatusChip } from '@/components/ui/status-chip';
import { iykeMcpInfo, isRemoteWebSession } from '@/lib/tauri-cmd';

import { SettingRow } from './setting-row';
import { copyText } from '@/lib/clipboard';

export const DESKTOP_ONLY_IYKE_MCP =
	'Desktop app only. The iyke MCP server runs beside the Ikenga desktop app; open Settings there to copy its path and client config.';

// Iyke MCP section — surfaces the absolute path of the bundled MCP server
// binary so external clients (Claude Desktop, Cursor) can spawn it directly.
// The path is stable for a given install; on shell upgrades the resource
// dir typically stays the same on Linux/macOS, so configs remain valid.
export function IykeMcpSection() {
	// Browser session (gap audit rank 15): the binary lives on the daemon's
	// host, `iyke_mcp_info` is not served, and the old fallback blamed "a
	// normal install". Say what is true instead, and don't fire the RPC.
	const remote = isRemoteWebSession();
	const info = useQuery({
		queryKey: ['iyke-mcp-info'],
		queryFn: iykeMcpInfo,
		staleTime: 30_000,
		enabled: !remote,
	});
	const [copied, setCopied] = useState<null | 'path' | 'json'>(null);

	if (remote) {
		return <FeedbackState variant="empty" body={DESKTOP_ONLY_IYKE_MCP} className="min-h-0 py-4" />;
	}

	if (info.isLoading) {
		return (
			<FeedbackState variant="loading" body="Resolving binary path…" className="min-h-0 py-4" />
		);
	}

	const data = info.data;
	if (!data?.path) {
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
		if (await copyText(text)) {
			setCopied(kind);
			setTimeout(() => setCopied((c) => (c === kind ? null : c)), 1500);
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
