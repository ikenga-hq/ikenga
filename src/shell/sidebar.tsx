import type React from 'react';
import { useShellStore } from '@/lib/shell/shell-store';
import { SettingsMode } from './sidebar-modes/settings-mode';
import { Explorer } from './explorer/explorer';

export function Sidebar() {
	const activeMode = useShellStore((s) => s.activeMode);

	if (activeMode === 'chi' || activeMode === 'ngwa') {
		return null;
	}

	switch (activeMode) {
		case 'settings':
			return renderSidebar('Settings', <SettingsMode />);
		case 'project':
		default:
			return (
				<nav
					aria-label="Explorer sidebar"
					className="flex h-full flex-col border-r border-border bg-card"
				>
					<Explorer />
				</nav>
			);
	}
}

/** The sidebar chrome — workspace-tinted head + scrollable body. */
function renderSidebar(title: string, body: React.ReactNode) {
	return (
		<nav
			aria-label={`${title} sidebar`}
			className="flex h-full flex-col border-r border-border bg-card"
			style={{
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
