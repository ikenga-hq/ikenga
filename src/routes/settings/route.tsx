import { Copy, Terminal } from 'lucide-react';
import { useMemo, useState } from 'react';
import {
	Outlet,
	createFileRoute,
	redirect,
	useNavigate,
	useRouterState,
} from '@tanstack/react-router';

import { useShellStore } from '@/lib/shell/shell-store';
import {
	SettingsNav,
	settingsIykeLine,
	settingsSection,
	type SettingsSectionId,
	type SettingsScopeId,
} from '@/shell/settings/nav';
import {
	SettingsSectionProvider,
	useSettingsDocument,
} from '@/shell/settings/field';
import { SettingsSectionHeader } from '@/shell/settings/header';
import { SettingsSearchResults } from '@/shell/settings/search';

export const Route = createFileRoute('/settings')({
	beforeLoad: ({ location }) => {
		if (location.pathname === '/settings' || location.pathname === '/settings/') {
			throw redirect({ to: '/settings/appearance' });
		}
	},
	component: SettingsLayout,
});

function sectionIdFromPath(pathname: string): SettingsSectionId {
	const segment = pathname.replace(/^\/settings\/?/, '').split('/')[0];
	return settingsSection(segment).id;
}

function SettingsLayout() {
	const navigate = useNavigate();
	const [scope, setScope] = useState<SettingsScopeId>('personal');
	const [search, setSearch] = useState('');
	const activeProjectId = useShellStore((s) => s.activeProjectId);
	const projects = useShellStore((s) => s.projects);
	const activeRoot = projects.find((p) => p.id === activeProjectId)?.root_path ?? null;
	const projectId = scope === 'project' ? activeProjectId : null;
	const document = useSettingsDocument(scope, projectId);
	const pathname = useRouterState({ select: (state) => state.location.pathname });
	const activeId = sectionIdFromPath(pathname);
	const overrides = useMemo(
		() => new Set(scope === 'project' ? (document.result?.overrides ?? []) : []),
		[scope, document.result]
	);
	const searchActive = search.trim().length > 0;
	const projectRoot = scope === 'project' ? activeRoot : null;

	const context = useMemo(
		() => ({
			scope,
			setScope,
			projectId,
			projectRoot,
			result: document.result,
			overrides,
			isLoading: document.isLoading,
			refresh: document.refresh,
		}),
		[scope, projectId, projectRoot, document.result, overrides, document.isLoading, document.refresh]
	);

	function goToSection(id: SettingsSectionId) {
		setSearch('');
		void navigate({ to: `/settings/${id}` });
	}

	const iykeLine = settingsIykeLine(activeId, scope);

	// D-06 (Actions, menus and keys, WP-57) is its own pane-hosted full-bleed
	// surface with its own header and tab bar — it does not fit the generic
	// SettingsSectionHeader (bound to SETTINGS_SECTIONS) or the left section
	// nav, so it renders outside this shell. D-03 Workspace links out to it
	// instead of listing it as a section.
	if (pathname.startsWith('/settings/actions')) {
		return <Outlet />;
	}

	return (
		<SettingsSectionProvider value={context}>
			<div className="flex h-full min-h-0">
				<SettingsNav
					activeId={activeId}
					search={search}
					onSearchChange={setSearch}
					onSelect={goToSection}
				/>
				<div className="flex min-w-0 flex-1 flex-col">
					<SettingsSectionHeader sectionId={activeId} searchActive={searchActive} />
					<div className="min-h-0 flex-1 overflow-y-auto">
						{searchActive ? (
							<SettingsSearchResults query={search} onGo={goToSection} />
						) : (
							<Outlet />
						)}
					</div>
					{!searchActive && (
						<div className="flex h-8 shrink-0 items-center gap-2 border-t border-border-soft bg-[var(--bg-sunken)] px-5 font-mono text-[10px] text-muted-foreground">
							<Terminal className="h-3 w-3 shrink-0" />
							<span className="truncate text-foreground">{iykeLine}</span>
							<button
								type="button"
								className="ml-auto shrink-0 rounded p-1 outline-none transition-colors hover:text-foreground focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary"
								aria-label="Copy the iyke command"
								onClick={() => void navigator.clipboard.writeText(iykeLine).catch(() => {})}
							>
								<Copy className="h-3 w-3" />
							</button>
						</div>
					)}
				</div>
			</div>
		</SettingsSectionProvider>
	);
}
