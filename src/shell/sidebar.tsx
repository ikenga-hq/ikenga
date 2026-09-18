import { usePkgActivityBarEntries } from '@/lib/pkg/use-activity-bar-entries';
import {
	type CoreMode, // TODO(WP-04)
	isPkgMode, // TODO(WP-04): deprecated, never true after v16
	type LegacyActivityMode, // TODO(WP-04)
	pkgIdFromMode, // TODO(WP-04)
	useShellStore,
} from '@/lib/shell/shell-store';
import { AppMode } from './sidebar-modes/app-mode';
import { ArtifactGridMode } from './sidebar-modes/artifact-grid-mode';
import { FilesMode } from './sidebar-modes/files-mode';
import { NgwaMode } from './sidebar-modes/ngwa-mode';
import { PkgMode } from './sidebar-modes/pkg-mode';
import { PkgsMode } from './sidebar-modes/pkgs-mode';
import { SettingsMode } from './sidebar-modes/settings-mode';

const CORE_TITLES = {
	app: 'Ikenga',
	files: 'Files',
	'artifact-grid': 'Artifact grid',
	ngwa: 'Ngwa',
	pkgs: 'Packages',
	settings: 'Settings',
} as const;

export function Sidebar() {
	// TODO(WP-04): widened cast so the pre-v16 switch below still compiles (g-state.md §6).
	const activeMode = useShellStore((s) => s.activeMode) as CoreMode | LegacyActivityMode;
	// Entries are only consulted for the head title when in a pkg mode; the
	// hook is cheap (one cached kernel snapshot) and safe to always call.
	const { entries: pkgEntries } = usePkgActivityBarEntries();

	let title: string = CORE_TITLES.app;
	let body: React.ReactNode;

	if (isPkgMode(activeMode)) {
		// A pkg owns the sidebar in its own mode — render its published menu.
		// Title is the pkg's activity-bar label, falling back to the raw id if
		// the snapshot hasn't loaded the entry yet.
		const pkgId = pkgIdFromMode(activeMode) ?? '';
		const entry = pkgEntries.find((e) => e.pkg_id === pkgId);
		title = entry?.label ?? pkgId;
		body = <PkgMode pkgId={pkgId} />;
		return renderSidebar(title, body);
	}

	switch (activeMode) {
		case 'app':
			title = CORE_TITLES.app;
			body = <AppMode />;
			break;
		case 'files':
			title = CORE_TITLES.files;
			body = <FilesMode />;
			break;
		case 'artifact-grid':
			title = CORE_TITLES['artifact-grid'];
			body = <ArtifactGridMode />;
			break;
		case 'ngwa':
			title = CORE_TITLES.ngwa;
			body = <NgwaMode />;
			break;
		case 'pkgs':
			title = CORE_TITLES.pkgs;
			body = <PkgsMode />;
			break;
		case 'settings':
			title = CORE_TITLES.settings;
			body = <SettingsMode />;
			break;
		default:
			// Should be unreachable post-strip — CoreMode is a closed union of
			// 4 variants. Keeps the compiler honest if the union ever widens.
			title = CORE_TITLES.app;
			body = <AppMode />;
	}

	return renderSidebar(title, body);
}

/** The sidebar chrome — workspace-tinted head + scrollable body. Shared by the
 *  CORE-mode switch and the `pkg:<id>` path so they render identically. */
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
