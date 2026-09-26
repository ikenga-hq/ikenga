// D-06 tab bar (WP-57): Actions · Editor · Menus · Keys (`import` is
// routable but not shown here — item 19). Item 13 conformance: keys 1–4
// switch tabs when focus is outside a text input, arrow keys rove focus
// between the tab buttons (roving tabindex, WAI-ARIA tabs pattern), and the
// right-side note reads "N actions · M yours".

import { useEffect, useRef } from 'react';
import type { KeyboardEvent as ReactKeyboardEvent } from 'react';
import { Link, useNavigate } from '@tanstack/react-router';
import { Bolt, FileCode, Keyboard, ListTree } from 'lucide-react';
import type { EffectiveModel } from '@/lib/actions/store';
import { ACTIONS_VISIBLE_TABS, type VisibleActionsTabId } from './types';

const TAB_META: Record<VisibleActionsTabId, { label: string; icon: typeof Bolt }> = {
	actions: { label: 'Actions', icon: Bolt },
	editor: { label: 'Editor', icon: FileCode },
	menus: { label: 'Menus', icon: ListTree },
	keys: { label: 'Keys', icon: Keyboard },
};

const TABS = ACTIONS_VISIBLE_TABS.map((id) => ({ id, ...TAB_META[id] }));

function isTypingTarget(target: EventTarget | null): boolean {
	const el = target as HTMLElement | null;
	if (!el) return false;
	return el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable;
}

export interface ActionsTabsProps {
	/** `null` when the active surface (e.g. `import`) is not one of the 1–4
	 *  visible tabs — no tab bar entry highlights, but 1–4 still switch. */
	activeTab: VisibleActionsTabId | null;
	model: EffectiveModel;
}

export function ActionsTabs({ activeTab, model }: ActionsTabsProps) {
	const navigate = useNavigate();
	const tabRefs = useRef<Array<HTMLAnchorElement | null>>([]);
	const barRef = useRef<HTMLDivElement | null>(null);

	function go(tab: VisibleActionsTabId) {
		void navigate({ to: '/settings/actions/$tab', params: { tab } });
	}

	// Keys 1–4: switch tabs whenever focus is inside this Actions view and
	// outside a text input. Panes keep routes alive, so a document-wide
	// listener would take the digits from every other pane in the app.
	useEffect(() => {
		function onKeyDown(e: KeyboardEvent) {
			if (e.metaKey || e.ctrlKey || e.altKey || isTypingTarget(e.target)) return;
			const root = barRef.current?.closest('.view-acts');
			if (!root || !(e.target instanceof Node) || !root.contains(e.target)) return;
			const index = ['1', '2', '3', '4'].indexOf(e.key);
			if (index === -1) return;
			const tab = TABS[index];
			if (!tab || tab.id === activeTab) return;
			e.preventDefault();
			void navigate({ to: '/settings/actions/$tab', params: { tab: tab.id } });
		}
		document.addEventListener('keydown', onKeyDown);
		return () => document.removeEventListener('keydown', onKeyDown);
	}, [activeTab, navigate]);

	// With `import` active no tab is selected, so the first tab holds the tab
	// stop and arrows move from whichever tab has focus.
	const stopIndex = Math.max(0, TABS.findIndex((t) => t.id === activeTab));

	function onTabsKeyDown(e: ReactKeyboardEvent) {
		const focused = tabRefs.current.findIndex((el) => el === document.activeElement);
		const current = focused === -1 ? stopIndex : focused;
		let next = current;
		if (e.key === 'ArrowRight') next = (current + 1) % TABS.length;
		else if (e.key === 'ArrowLeft') next = (current - 1 + TABS.length) % TABS.length;
		else if (e.key === 'Home') next = 0;
		else if (e.key === 'End') next = TABS.length - 1;
		else return;
		e.preventDefault();
		tabRefs.current[next]?.focus();
		go(TABS[next].id);
	}

	const userCount = model.actions.filter((a) => a.source === 'personal' || a.source === 'project').length;
	const note = `${model.actions.length} actions · ${userCount} yours`;

	return (
		<div ref={barRef} className="ntabs" role="tablist" aria-label="Actions surfaces" onKeyDown={onTabsKeyDown}>
			{TABS.map((t, i) => (
				<Link
					key={t.id}
					ref={(el) => {
						tabRefs.current[i] = el;
					}}
					to="/settings/actions/$tab"
					params={{ tab: t.id }}
					className={`ntab ${activeTab === t.id ? 'on' : ''}`}
					role="tab"
					tabIndex={i === stopIndex ? 0 : -1}
					aria-selected={activeTab === t.id}
				>
					<t.icon className="h-3.5 w-3.5" />
					<span>{t.label}</span>
					<span className="k">{i + 1}</span>
				</Link>
			))}
			<span className="far">
				<span className="meta mono">{note}</span>
			</span>
		</div>
	);
}
