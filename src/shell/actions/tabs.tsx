// D-06 tab bar (WP-57): Actions · Editor · Menus · Keys, keys 1–4 (display
// hint only, same as Ngwa's own tab strip — the physical 1–4 keys are wired
// once the frame keymap covers this pane, not this WP's scope). Each tab is
// its own route (`/settings/actions/$tab`), not client-only state, so the
// URL, back/forward and a direct link all work.

import { Link } from '@tanstack/react-router';
import { Bolt, FileCode, Keyboard, ListTree } from 'lucide-react';
import type { ActionsTabId } from './types';

const TABS: ReadonlyArray<{ id: ActionsTabId; label: string; icon: typeof Bolt }> = [
	{ id: 'actions', label: 'Actions', icon: Bolt },
	{ id: 'editor', label: 'Editor', icon: FileCode },
	{ id: 'menus', label: 'Menus', icon: ListTree },
	{ id: 'keys', label: 'Keys', icon: Keyboard },
];

export interface ActionsTabsProps {
	activeTab: ActionsTabId;
}

export function ActionsTabs({ activeTab }: ActionsTabsProps) {
	return (
		<div className="ntabs" role="tablist" aria-label="Actions surfaces">
			{TABS.map((t, i) => (
				<Link
					key={t.id}
					to="/settings/actions/$tab"
					params={{ tab: t.id }}
					className={`ntab ${activeTab === t.id ? 'on' : ''}`}
					role="tab"
					aria-selected={activeTab === t.id}
				>
					<t.icon className="h-3.5 w-3.5" />
					<span>{t.label}</span>
					<span className="k">{i + 1}</span>
				</Link>
			))}
		</div>
	);
}
