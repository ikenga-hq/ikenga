import { useShellStore } from '@/lib/shell/shell-store';
import { AppMode } from './sidebar-modes/app-mode';
import { NgwaMode } from './sidebar-modes/ngwa-mode';
import { SettingsMode } from './sidebar-modes/settings-mode';

// WP-04 replaces this whole switch with the Explorer. Until then each v16
// mode keeps the body it already rendered on the integration branch
// (g-state.md §6 "interim behaviour"): Project and Chi show the App menu,
// Ngwa its Claude-config browser, Settings the settings nav. The pre-v16
// arms (files, artifact-grid, pkgs, `pkg:<id>`) were unreachable once the
// store stopped holding those modes, and went with the compat shim (WP-03).
const TITLES = {
	project: 'Ikenga',
	chi: 'Ikenga',
	ngwa: 'Ngwa',
	settings: 'Settings',
} as const;

export function Sidebar() {
	const activeMode = useShellStore((s) => s.activeMode);

	let body: React.ReactNode;
	switch (activeMode) {
		case 'ngwa':
			body = <NgwaMode />;
			break;
		case 'settings':
			body = <SettingsMode />;
			break;
		default:
			body = <AppMode />;
	}

	return renderSidebar(TITLES[activeMode] ?? TITLES.project, body);
}

/** The sidebar chrome — workspace-tinted head + scrollable body. */
function renderSidebar(title: string, body: React.ReactNode) {
	return (
		<nav
			aria-label={`${title} sidebar`}
			className="flex h-full flex-col border-r border-border bg-card"
			// Workspace-tinted gradient on the head, fading into surface (shell.css §sidebar-head).
			style={{
				// Re-resolve --tint-bg-active per workspace via the [data-workspace] attribute on <html>.
				// No JS branching needed — the var cascades.
				['--ikenga-sidebar-tint' as string]: 'var(--tint-bg-active, var(--bg-surface))',
			}}
		>
			<div
				className="flex h-12 shrink-0 items-center border-b border-border-soft px-4"
				style={{
					background:
						'linear-gradient(180deg, var(--tint-bg-active, var(--bg-surface)) 0%, var(--bg-surface) 100%)',
				}}
			>
				<span
					className="text-sm font-medium tracking-tight"
					style={{
						color: 'var(--fg)',
						fontFamily: 'var(--font-display)',
						fontSize: 'var(--text-h3)',
					}}
				>
					{title}
				</span>
			</div>
			<div className="flex-1 overflow-hidden">{body}</div>
		</nav>
	);
}
