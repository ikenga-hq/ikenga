import { createFileRoute } from '@tanstack/react-router';
import { Plug } from 'lucide-react';

// WP-35 ships the section frame only; Supabase, connector cards, iyke MCP
// status and bridge API keys are rebuilt on this substrate in WP-36.

function IntegrationsPage() {
	return (
		<div className="mx-auto w-full max-w-[720px] space-y-6 px-6 py-6">
			<header className="space-y-1">
				<h2
					className="text-2xl font-semibold tracking-tight"
					style={{ fontFamily: 'var(--font-display)' }}
				>
					Integrations
				</h2>
				<p className="max-w-prose text-sm leading-relaxed text-muted-foreground">
					Supabase, connectors, iyke MCP and bridge API keys.
				</p>
			</header>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Connections
					</h3>
				</header>
				<div className="flex items-center gap-3 px-4 py-6 text-sm text-muted-foreground">
					<Plug className="h-4 w-4 shrink-0" />
					<span>
						The integrations surface is being rebuilt on this shell — connectors and bridge keys
						arrive with the next update.
					</span>
				</div>
			</section>
		</div>
	);
}

export const Route = createFileRoute('/settings/integrations')({
	component: IntegrationsPage,
});
