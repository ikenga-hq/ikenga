// /chi/ — Chi dispatch landing (WP-10).
//
// The central landing for the Chi noun (⌘2 / `iyke go /chi`). Displays
// companion dispatch overview, active sessions, and dispatch prompt targets.

import { createFileRoute } from '@tanstack/react-router';
import { Bot, Sparkles, Terminal, ArrowRight } from 'lucide-react';
import { useTerminalStore } from '@/terminal/session-store';
import { useShellStore } from '@/lib/shell/shell-store';

function ChiPage() {
	const activeProject = useShellStore((s) => s.activeProject);
	const tabs = useTerminalStore((s) => s.tabs);
	const activeCount = tabs.filter((t) => t.status === 'running' || t.status === 'spawning').length;

	return (
		<div className="flex h-full flex-col bg-background text-foreground overflow-y-auto">
			<div className="mx-auto w-full max-w-2xl px-6 py-12">
				<div className="mb-8 flex items-center gap-3">
					<div className="flex h-10 w-10 items-center justify-center rounded-lg bg-primary/10 text-primary">
						<Bot className="h-6 w-6" />
					</div>
					<div>
						<h1 className="text-xl font-semibold tracking-tight">
							Chi — Companion Dispatch
							{activeProject && (
								<span className="text-sm font-normal text-muted-foreground ml-2">
									({activeProject.id})
								</span>
							)}
						</h1>
						<p className="text-sm text-muted-foreground">
							Your project intelligence companion. Brief Chi to execute tasks, inspect files, and automate workflows.
						</p>
					</div>
				</div>

				<div className="grid gap-4 sm:grid-cols-2">
					<div className="rounded-lg border border-border bg-card p-5 shadow-xs">
						<div className="mb-3 flex items-center gap-2 text-primary font-medium text-sm">
							<Terminal className="h-4 w-4" />
							<span>Active Sessions</span>
						</div>
						<p className="text-2xl font-bold">{activeCount}</p>
						<p className="mt-1 text-xs text-muted-foreground">
							{activeCount === 1
								? '1 active terminal session running'
								: `${activeCount} active terminal sessions running`}
						</p>
					</div>

					<div className="rounded-lg border border-border bg-card p-5 shadow-xs">
						<div className="mb-3 flex items-center gap-2 text-primary font-medium text-sm">
							<Sparkles className="h-4 w-4" />
							<span>Dispatch Surface</span>
						</div>
						<p className="text-xs text-muted-foreground">
							The Chi dispatch bar lives in the right Companion panel (⌘2). Dispatches target the focused terminal or a fresh engine worker.
						</p>
					</div>
				</div>

				<div className="mt-8 rounded-lg border border-border/80 bg-muted/40 p-5">
					<h2 className="text-sm font-semibold mb-2">How to dispatch with Chi</h2>
					<ul className="space-y-2 text-xs text-muted-foreground">
						<li className="flex items-start gap-2">
							<ArrowRight className="h-3.5 w-3.5 mt-0.5 shrink-0 text-primary" />
							<span>Press <kbd className="rounded border bg-muted px-1.5 py-0.5 font-mono text-[10px]">⌘2</kbd> or click Chi on the rail to bring the Companion forward.</span>
						</li>
						<li className="flex items-start gap-2">
							<ArrowRight className="h-3.5 w-3.5 mt-0.5 shrink-0 text-primary" />
							<span>Right-click any file, artifact, or session in the Explorer and choose <em>Hand to Chi</em>.</span>
						</li>
						<li className="flex items-start gap-2">
							<ArrowRight className="h-3.5 w-3.5 mt-0.5 shrink-0 text-primary" />
							<span>Use <code className="font-mono text-[10px] bg-muted px-1 rounded">iyke chi dispatch "&lt;task&gt;"</code> from your terminal or scripts.</span>
						</li>
					</ul>
				</div>
			</div>
		</div>
	);
}

export const Route = createFileRoute('/chi/')({
	component: ChiPage,
});
