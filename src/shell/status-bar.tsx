// WP-09 — the status bar, "the truth line" (D-01, `designs/frame-workbench-v4.html`
// `.status`; spec §3.13 amended by §6A.7 / §6A.8 and v4 P5).
//
//   left   branch · modified · project
//   middle Ngwa: installed · updates · violations   (each a deep link)
//   right  [notifications bell slot] · permissions · runs · session cost ·
//          engine · shortcuts
//
// Counts render only when non-zero (§6A.8): a zero segment is not rendered at
// all, and the Ngwa group disappears when all three are zero. Project, cost
// and engine are read-only (P5 — they are switched 40px away, in the project
// chip and the Companion's target chip), so they get no tab stop. The
// buttons form one composite tab stop with ←/→/Home/End roving (§3.13). The
// permission and run counts are also announced through a polite live region;
// cost churns too fast to announce.
//
// Data sources — all existing, nothing new on the host:
//   · branch / modified — `useGitRepoSummary()` (git pkg `repo.snapshot`)
//   · Ngwa counts — `usePkgsDerived()`, the pkg kernel snapshot + registry
//     index + violations list the pkg surface already reads (Phase 2 swaps in
//     `ngwa_snapshot`)
//   · permissions — the exact query + 15 s poll the rail's
//     `ApprovalsRailButton` used (`paActionsListQueryOptions()`), moved here
//     with the rail button's removal (WP-03)
//   · runs — agent terminals whose PTY is running (terminal store)
//   · session cost — the Claude statusline snapshots `CostHud` reads
//   · engine — the Companion's next-dispatch target, else the default engine

import { useQuery } from '@tanstack/react-query';
import { Folder, GitBranch, HelpCircle, Package, ShieldCheck } from 'lucide-react';
import {
	Fragment,
	type KeyboardEvent as ReactKeyboardEvent,
	type ReactNode,
	useEffect,
	useRef,
	useState,
} from 'react';
import { cn } from '@/components/ui/utils';
import { iykeFetch } from '@/lib/iyke/client';
import { labelFor } from '@/lib/keymap/registry';
import { usePaneStore } from '@/lib/panes/pane-store';
import { usePkgsDerived } from '@/lib/pkgs/use-derived';
import { paActionsListQueryOptions } from '@/lib/queries/pa-actions';
import { useShellStore } from '@/lib/shell/shell-store';
import { listen } from '@/lib/transport';
import { useTerminalStore } from '@/terminal/session-store';
import type { StatuslineSnapshot } from '@/terminal/cost-hud';
import { openCommandPalette } from './command-palette';
import { useCompanionStore } from './companion/companion-store';
import { GIT_BRANCHES_ROUTE, GIT_CHANGES_ROUTE, useGitRepoSummary } from './title-row';
// WP-41 (D-07 update-flow) — the one status-bar edit this WP makes: swap the
// plain engine segment for a component that also mirrors a live shell-update
// download (see that file's header for why it needs its own store).
import { UpdaterStatusBarProgress } from './updater/status-bar-slot';

/** Ngwa deep links. Phase 1 lands on the pkg surface's matching filter; the
 *  `/ngwa/*` routes (WP-10) take over these targets when they exist. */
export const NGWA_LINKS = {
	installed: '/packages?filter=installed',
	updates: '/packages?filter=updates',
	violations: '/packages?filter=review',
} as const;

export const APPROVALS_ROUTE = '/outbox/approvals';
/** Same poll the rail's approvals button ran (activity-bar.tsx). */
export const APPROVALS_REFETCH_MS = 15_000;

function plural(n: number, one: string, many = `${one}s`): string {
	return `${n} ${n === 1 ? one : many}`;
}

// ─── data hooks ────────────────────────────────────────────────────────────

/** Pending approve-gate drafts. Identical options to the rail's former
 *  `ApprovalsRailButton` — same key, so the route's commit/reject
 *  invalidations refresh this count too. */
export function usePendingApprovalsCount(): number {
	const { data } = useQuery({
		...paActionsListQueryOptions(),
		refetchInterval: APPROVALS_REFETCH_MS,
	});
	return data?.length ?? 0;
}

/** Agent terminals whose PTY is live — the run segment. */
function useLiveRunIds(): string[] {
	const tabs = useTerminalStore((s) => s.tabs);
	return tabs
		.filter((t) => t.status === 'running' && (!!t.spec.wrap || !!t.claudeSessionId))
		.map((t) => t.id);
}

/** Latest statusline snapshot per terminal id — the same feed `CostHud`
 *  renders per terminal, aggregated here. */
function useStatuslineSnapshots(): Record<string, StatuslineSnapshot> {
	const [snaps, setSnaps] = useState<Record<string, StatuslineSnapshot>>({});
	useEffect(() => {
		let cancelled = false;
		iykeFetch('/iyke/statusline/snapshot')
			.then((res) => (res.ok ? res.json() : null))
			.then((data: Record<string, StatuslineSnapshot> | null) => {
				if (!cancelled && data && typeof data === 'object') setSnaps(data);
			})
			.catch(() => {});
		let unlisten: (() => void) | undefined;
		listen<StatuslineSnapshot>('statusline://snapshot', (event) => {
			const id = event.payload?.ikenga_terminal_id;
			if (!id) return;
			setSnaps((prev) => ({ ...prev, [id]: event.payload }));
		})
			.then((fn) => {
				if (cancelled) fn();
				else unlisten = fn;
			})
			.catch(() => {});
		return () => {
			cancelled = true;
			unlisten?.();
		};
	}, []);
	return snaps;
}

/** Engine the next dispatch goes to (Companion target, else the default). */
function useNextEngineId(): string | null {
	const target = useShellStore((s) => s.companion.activeTarget);
	const defaultEngineId = useShellStore((s) => s.defaultEngineId);
	if (target.kind !== 'session' && target.engine_id) return target.engine_id;
	return defaultEngineId;
}

// ─── presentation ──────────────────────────────────────────────────────────

const ITEM =
	'flex h-5 items-center gap-1 rounded-[var(--radius-xs)] px-2 text-muted-foreground outline-none';
const BUTTON = cn(
	ITEM,
	'transition-colors duration-[var(--motion-fast)] ease-[var(--ease-calm)] motion-reduce:transition-none hover:bg-[var(--bg-raised)] hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset'
);

interface SegButtonProps {
	id: string;
	rovingId: string | null;
	onClick: () => void;
	label: string;
	title?: string;
	className?: string;
	children: ReactNode;
}

function SegButton({ id, rovingId, onClick, label, title, className, children }: SegButtonProps) {
	return (
		<button
			type="button"
			data-seg={id}
			tabIndex={rovingId === id ? 0 : -1}
			onClick={onClick}
			aria-label={label}
			title={title}
			className={cn(BUTTON, className)}
		>
			{children}
		</button>
	);
}

function ReadOnly({ id, title, children }: { id: string; title: string; children: ReactNode }) {
	return (
		<span data-seg={id} title={title} className={cn(ITEM, 'cursor-default')}>
			{children}
		</span>
	);
}

/** Phase 5 (D-07) mounts the notifications bell here. Empty until then. */
export function NotificationsBellSlot() {
	return <span data-slot="notifications-bell" className="contents" />;
}

export function StatusBar() {
	const navigateFocused = usePaneStore((s) => s.navigateFocused);
	const git = useGitRepoSummary().data ?? null;
	const project = useShellStore((s) => s.projects.find((p) => p.id === s.activeProjectId) ?? null);
	const pkgs = usePkgsDerived();
	const approvals = usePendingApprovalsCount();
	const runIds = useLiveRunIds();
	const snaps = useStatuslineSnapshots();
	const engine = useNextEngineId();

	const installed = pkgs.installed.length;
	const updates = pkgs.updates.length;
	const violations = pkgs.violations.length;
	const runs = runIds.length;
	const cost = runIds.reduce((sum, id) => sum + (snaps[id]?.cost?.total_cost_usd ?? 0), 0);
	const shortcutsKey = labelFor('shortcuts.open');

	// Ngwa: up to three deep-link segments, zero segments dropped (§6A.7).
	const ngwaSegments = [
		{
			id: 'ngwa-installed',
			count: installed,
			text: `${installed} installed`,
			to: NGWA_LINKS.installed,
		},
		{ id: 'ngwa-updates', count: updates, text: plural(updates, 'update'), to: NGWA_LINKS.updates },
		{
			id: 'ngwa-violations',
			count: violations,
			text: plural(violations, 'violation'),
			to: NGWA_LINKS.violations,
			className: 'text-[var(--danger)]',
		},
	].filter((seg) => seg.count > 0);

	// Ids of the focusable segments, in visual order — the roving set.
	const buttonIds = [
		git && 'branch',
		git && git.modified > 0 && 'modified',
		...ngwaSegments.map((seg) => seg.id),
		approvals > 0 && 'permissions',
		runs > 0 && 'runs',
		'shortcuts',
	].filter((v): v is string => typeof v === 'string');

	const [roving, setRoving] = useState<string | null>(null);
	const rovingId = roving && buttonIds.includes(roving) ? roving : (buttonIds[0] ?? null);
	const barRef = useRef<HTMLDivElement | null>(null);

	function focusSeg(id: string) {
		setRoving(id);
		barRef.current?.querySelector<HTMLButtonElement>(`button[data-seg="${id}"]`)?.focus();
	}

	function onKeyDown(e: ReactKeyboardEvent<HTMLDivElement>) {
		const current = (e.target as HTMLElement).closest<HTMLElement>('button[data-seg]')?.dataset.seg;
		const idx = current ? buttonIds.indexOf(current) : -1;
		if (idx < 0) return;
		let next: number | null = null;
		if (e.key === 'ArrowRight') next = (idx + 1) % buttonIds.length;
		else if (e.key === 'ArrowLeft') next = (idx - 1 + buttonIds.length) % buttonIds.length;
		else if (e.key === 'Home') next = 0;
		else if (e.key === 'End') next = buttonIds.length - 1;
		if (next === null) return;
		e.preventDefault();
		focusSeg(buttonIds[next]!);
	}

	const liveText = [
		approvals > 0 ? `${plural(approvals, 'permission')} pending` : '',
		runs > 0 ? `${plural(runs, 'run')} live` : '',
	]
		.filter(Boolean)
		.join(', ');

	return (
		<div
			ref={barRef}
			role="toolbar"
			aria-label="Status bar"
			aria-orientation="horizontal"
			data-testid="status-bar"
			onKeyDown={onKeyDown}
			onFocus={(e) => {
				const seg = (e.target as HTMLElement).dataset?.seg;
				if (seg && buttonIds.includes(seg)) setRoving(seg);
			}}
			className="flex h-[26px] flex-none items-center gap-1 border-t border-border bg-[var(--bg-sunken)] px-2 text-[length:var(--text-micro)] text-muted-foreground"
		>
			{/* ── left: branch · modified · project ── */}
			{git && (
				<SegButton
					id="branch"
					rovingId={rovingId}
					onClick={() => navigateFocused(GIT_BRANCHES_ROUTE)}
					label={`Branch ${git.branch} — open branches`}
				>
					<GitBranch aria-hidden className="h-3 w-3" />
					<span className="font-mono">{git.branch}</span>
				</SegButton>
			)}
			{git && git.modified > 0 && (
				<SegButton
					id="modified"
					rovingId={rovingId}
					onClick={() => navigateFocused(GIT_CHANGES_ROUTE)}
					label={`${git.modified} modified — show changes`}
					className="text-[var(--achievement)]"
				>
					<span className="font-mono">{git.modified} modified</span>
				</SegButton>
			)}
			{git && <span aria-hidden className="mx-2 h-3 w-px bg-border" />}
			{project && (
				<ReadOnly id="project" title="Active project — switch it from the project chip">
					<Folder aria-hidden className="h-3 w-3" />
					<span className="max-w-[12rem] truncate">{project.display_name}</span>
				</ReadOnly>
			)}

			{/* ── middle: Ngwa segments ── */}
			{ngwaSegments.length > 0 && (
				<span data-seg="ngwa" className="flex items-center">
					<Package aria-hidden className="mx-1 h-3 w-3" />
					<span className="sr-only">Ngwa:</span>
					{ngwaSegments.map((seg, i) => (
						<Fragment key={seg.id}>
							{i > 0 && (
								<span aria-hidden className="text-[var(--border-strong)]">
									·
								</span>
							)}
							<SegButton
								id={seg.id}
								rovingId={rovingId}
								onClick={() => navigateFocused(seg.to)}
								label={`Ngwa: ${seg.text}`}
								className={cn('px-1', seg.className)}
							>
								{seg.text}
							</SegButton>
						</Fragment>
					))}
				</span>
			)}

			{/* ── right ── */}
			<span className="ml-auto flex items-center gap-1">
				<NotificationsBellSlot />
				{approvals > 0 && (
					<SegButton
						id="permissions"
						rovingId={rovingId}
						onClick={() => navigateFocused(APPROVALS_ROUTE)}
						label={`${plural(approvals, 'permission')} pending — open approvals`}
						className="text-foreground"
					>
						<ShieldCheck aria-hidden className="h-3 w-3 text-[var(--live)]" />
						<span>{plural(approvals, 'permission')} pending</span>
					</SegButton>
				)}
				{runs > 0 && (
					<SegButton
						id="runs"
						rovingId={rovingId}
						onClick={() => useCompanionStore.getState().setState('expanded')}
						label={`${plural(runs, 'run')} live — open the Companion`}
						className="text-[var(--ember)]"
					>
						<span aria-hidden className="h-1.5 w-1.5 rounded-full bg-[var(--ember)]" />
						<span>{plural(runs, 'run')}</span>
					</SegButton>
				)}
				{cost > 0 && (
					<ReadOnly id="cost" title="Cost of the live agent sessions">
						<span>session</span>
						<span className="font-mono text-[var(--achievement)]">${cost.toFixed(2)}</span>
					</ReadOnly>
				)}
				{/* WP-41 (D-07 update-flow, 06-interaction-spec.md §3.13 #90): a live
				 * shell-update download replaces this segment; otherwise it's the
				 * same engine read-only segment as before. */}
				<UpdaterStatusBarProgress engine={engine} />
				<SegButton
					id="shortcuts"
					rovingId={rovingId}
					onClick={() => openCommandPalette('shortcuts')}
					label={`Keyboard shortcuts (${shortcutsKey})`}
				>
					<HelpCircle aria-hidden className="h-3 w-3" />
					<span className="font-mono">{shortcutsKey}</span>
				</SegButton>
			</span>

			<span role="status" aria-live="polite" className="sr-only">
				{liveText}
			</span>
		</div>
	);
}
