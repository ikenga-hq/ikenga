// Home ("Obi" — the hearth, the room you enter first) — composable free-form
// canvas. Shell built-ins (greeting, sessions, quick actions, scratchpad)
// live alongside pkg-contributed widgets (tasks, inbox, boards, finance).
// Read-only by default; `Customize` flips edit mode with a drag palette,
// drag-to-reposition, and layout persistence.
//
// Design source: design/shell/concepts/03-screens/16-home.html (concept) +
// 16-home.artifact.html (live artifact). This file is the shell-native port:
// no bridge polyfill, layout persisted via localStorage (will move to SQLite
// once a `home_layout` migration lands).
//
// WP-18c (plans/atelier-parity/designs/parity-obi-home-live.html) ported
// tasks/inbox/finance onto real `lib/queries/home-widgets.ts` queries and
// quick actions onto the shared skill-actions query; boards stays a "pkg
// installed?" check (no boards/frames table exists — see `BoardsBody`).
// Sessions + scratchpad were already live before this WP.

import { useEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from 'react';
import { useQuery } from '@tanstack/react-query';
import {
	Canvas,
	type CanvasHandle,
	type ItemId,
	type ItemRenderState,
	type Placement,
	type Viewport,
} from '@ikenga/contract/canvas';
import { dispatchAction, isDispatchable } from '@/components/pkg/actions/action-runner';
import { dailyAddress, quoteOfTheDay, partOfDay } from '@/lib/lore';
import { usePaneStore } from '@/lib/panes/pane-store';
import {
	homeBoardsPkgStatusQueryOptions,
	homeFinanceQueryOptions,
	homeInboxQueryOptions,
	homeTasksQueryOptions,
	RUNWAY_TARGET_MONTHS,
	type HomeInboxRow,
	type HomeTaskRow,
} from '@/lib/queries/home-widgets';
import { listScratchpads } from '@/lib/iyke/memory';
import { useAllSkillActions } from '@/shell/palette-actions';
import type { SkillAction } from '@/lib/tauri-cmd';

// ───────────────────────── icons ─────────────────────────

const Icons = {
	clock: (
		<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden={true}>
			<circle cx="12" cy="12" r="9" />
			<path d="M12 7v5l3 3" />
		</svg>
	),
	quick: (
		<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden={true}>
			<polyline points="9 11 12 14 22 4" />
			<path d="M21 12v7a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11" />
		</svg>
	),
	pad: (
		<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden={true}>
			<path d="M4 4h12l4 4v12H4z" />
			<polyline points="14 4 14 8 20 8" />
		</svg>
	),
	tasks: (
		<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden={true}>
			<rect x="4" y="4" width="16" height="16" rx="2" />
			<polyline points="8 12 11 15 16 9" />
		</svg>
	),
	mail: (
		<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden={true}>
			<rect x="3" y="5" width="18" height="14" rx="2" />
			<polyline points="3 7 12 13 21 7" />
		</svg>
	),
	studio: (
		<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden={true}>
			<rect x="3" y="5" width="18" height="14" rx="2" />
			<polyline points="9 9 15 12 9 15" />
		</svg>
	),
	finance: (
		<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden={true}>
			<polyline points="3 17 9 11 13 15 21 7" />
			<polyline points="14 7 21 7 21 14" />
		</svg>
	),
	cron: (
		<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden={true}>
			<circle cx="12" cy="12" r="9" />
			<line x1="12" y1="6" x2="12" y2="12" />
			<line x1="12" y1="12" x2="16" y2="14" />
		</svg>
	),
	agents: (
		<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden={true}>
			<circle cx="12" cy="9" r="3" />
			<path d="M6 21v-2a4 4 0 0 1 4-4h4a4 4 0 0 1 4 4v2" />
		</svg>
	),
	recenter: (
		<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden={true}>
			<polyline points="9 14 4 14 4 9" />
			<polyline points="15 10 20 10 20 15" />
			<polyline points="20 4 14 10" />
			<polyline points="4 20 10 14" />
		</svg>
	),
	edit: (
		<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden={true}>
			<path d="M3 17.25V21h3.75L17.81 9.94l-3.75-3.75L3 17.25z" />
		</svg>
	),
};

// ───────────────────────── layout types ─────────────────────────

type WidgetKind = 'greeting' | 'finance' | 'quick' | 'pad' | 'tasks' | 'inbox' | 'boards';

interface WidgetPlacement {
	id: string;
	kind: WidgetKind;
	x: number;
	y: number;
	w: number;
	h: number;
	patina?: 'warm';
}

// Default placement. Coordinates are canvas-space — the stage autofits to
// the viewport, scaling down only (never up).
const DEFAULT_LAYOUT: WidgetPlacement[] = [
	{ id: 'greeting', kind: 'greeting', x: 20, y: 30, w: 560, h: 320 },
	{ id: 'quick', kind: 'quick', x: 620, y: 30, w: 400, h: 240 },
	{ id: 'finance', kind: 'finance', x: 20, y: 380, w: 440, h: 260, patina: 'warm' },
	{ id: 'pad', kind: 'pad', x: 1060, y: 30, w: 320, h: 240 },
	{ id: 'tasks', kind: 'tasks', x: 620, y: 300, w: 360, h: 240 },
	{ id: 'inbox', kind: 'inbox', x: 1020, y: 300, w: 360, h: 240 },
	{ id: 'boards', kind: 'boards', x: 620, y: 570, w: 360, h: 200 },
];

const PALETTE_SHELL = [
	{ id: 'greeting', title: 'Greeting', icon: Icons.clock },
	{ id: 'quick', title: 'Quick actions', icon: Icons.quick },
	{ id: 'pad', title: 'Scratchpad', icon: Icons.pad },
	{ id: 'clock', title: 'World clock', icon: Icons.clock, placed: false },
];

const PALETTE_PKG = [
	{ id: 'tasks', title: "Today's tasks", icon: Icons.tasks, meta: 'tasks' },
	{ id: 'inbox', title: 'Inbox triage', icon: Icons.mail, meta: 'email' },
	{ id: 'boards', title: 'Active boards', icon: Icons.studio, meta: 'studio' },
	{ id: 'finance', title: 'Week so far', icon: Icons.finance, meta: 'finance' },
	{ id: 'cron', title: 'Scheduled runs', icon: Icons.cron, meta: 'cron', placed: false },
	{ id: 'agents', title: 'Agent runs', icon: Icons.agents, meta: 'agents', placed: false },
];

const LS_LAYOUT = 'ikenga:home:layout';

// ───────────────────────── shared widget-body state helpers ─────────────────
// WP-18c ports the four mock widgets (tasks / inbox / boards / finance) onto
// real `home-widgets.ts` queries. There is no CSS backing the shipped
// `home-widget`/`w-row`/`w-dot` DOM contract anywhere in the codebase yet
// (checked — not in styles.css, not in @ikenga/tokens); SessionsBody/PadBody
// already work around that with inline `style` objects for their loading/empty
// text, so the new bodies below follow the same convention rather than
// introducing global classnames (skeleton-row/w-error/na-cell) nothing else
// reads. See the WP-18c report for the full list of styling gaps this doesn't
// attempt to fix.

const CENTER_NOTE_STYLE: CSSProperties = {
	color: 'var(--fg-faint)',
	fontSize: 12,
	padding: '20px 0',
	textAlign: 'center',
};

function CenterNote({ children }: { children: ReactNode }) {
	return <div style={CENTER_NOTE_STYLE}>{children}</div>;
}

const SKELETON_ROW_STYLE: CSSProperties = {
	display: 'flex',
	alignItems: 'center',
	gap: 12,
	padding: '8px 0',
};
const SKELETON_DOT_STYLE: CSSProperties = {
	width: 7,
	height: 7,
	borderRadius: '50%',
	background: 'var(--bg-raised)',
	flexShrink: 0,
};
const SKELETON_BAR_STYLE: CSSProperties = {
	height: 9,
	borderRadius: 3,
	background: 'var(--bg-raised)',
};

/** Row-shaped loading placeholder (dot + label bar + meta bar). Matches the
 *  shipped `SessionsBody`/`PadBody` "Loading…" convention in spirit — plain,
 *  no shimmer keyframes (would need a new global CSS animation this file
 *  can't cheaply add without a stylesheet to hold it). */
function SkeletonRows({ count = 3 }: { count?: number }) {
	return (
		<>
			{Array.from({ length: count }).map((_, i) => (
				<div key={i} style={SKELETON_ROW_STYLE}>
					<span style={SKELETON_DOT_STYLE} />
					<span style={{ ...SKELETON_BAR_STYLE, flex: 1, width: '60%' }} />
					<span style={{ ...SKELETON_BAR_STYLE, width: 28 }} />
				</div>
			))}
		</>
	);
}

/** Danger-toned error panel + Retry, for the net-new `isError` handling none
 *  of the shipped widget bodies had before (design honesty note: "no widget
 *  body handles isError today"). */
function WidgetError({
	title,
	detail,
	onRetry,
}: {
	title: string;
	detail?: string;
	onRetry: () => void;
}) {
	return (
		<div
			style={{
				margin: '4px 0',
				padding: 10,
				borderRadius: 8,
				border: '1px solid var(--danger)',
				background: 'var(--danger-soft)',
				display: 'flex',
				flexDirection: 'column',
				gap: 8,
			}}
		>
			<div
				style={{
					display: 'flex',
					alignItems: 'center',
					gap: 8,
					fontSize: 12.5,
					fontWeight: 500,
					color: 'var(--fg)',
				}}
			>
				<svg
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					strokeWidth="1.6"
					style={{ width: 14, height: 14, color: 'var(--danger)', flexShrink: 0 }}
					aria-hidden={true}
				>
					<path d="M12 9v4" />
					<path d="M12 17h.01" />
					<circle cx="12" cy="12" r="9" />
				</svg>
				{title}
			</div>
			{detail && <div style={{ fontSize: 11.5, color: 'var(--fg-muted)' }}>{detail}</div>}
			<button
				type="button"
				onClick={onRetry}
				style={{
					alignSelf: 'flex-start',
					fontFamily: 'var(--font-body, inherit)',
					fontSize: 11.5,
					fontWeight: 500,
					padding: '3px 10px',
					borderRadius: 5,
					border: '1px solid var(--border)',
					background: 'var(--bg-surface)',
					color: 'var(--fg)',
					cursor: 'pointer',
				}}
			>
				Retry
			</button>
		</div>
	);
}

// ───────────────────────── widget bodies ─────────────────────────

// Status dot. The colour carries the state (live/warm/danger/agent), so the
// span also gets an accessible label + role="img" — colour alone fails WCAG
// 1.4.1 Use of Color. `cls` is the dotCls()/dot() modifier; `label` the
// human-readable state ('Live', 'Warm', 'Idle', …).
function DotSpan({ cls, label }: { cls: string; label: string }) {
	return <span className={`w-dot ${cls}`.trim()} role="img" aria-label={label} />;
}

function GreetingBody({ name }: { name: string }) {
	const [now, setNow] = useState(() => new Date());
	useEffect(() => {
		const t = setInterval(() => setNow(new Date()), 30_000);
		return () => clearInterval(t);
	}, []);
	const tod = dailyAddress(now);
	const q = quoteOfTheDay(now, 'daily-address');
	const shape = 'A clean slate.';
	return (
		<>
			<div
				style={{
					display: 'flex',
					alignItems: 'center',
					gap: 8,
					marginBottom: 12,
					fontFamily: 'var(--font-mono, ui-monospace)',
					fontSize: 10,
					letterSpacing: '0.16em',
					textTransform: 'uppercase',
					color: 'var(--fg-faint)',
				}}
			>
				<svg
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					strokeWidth="1.6"
					style={{ width: 16, height: 16, color: 'var(--achievement)' }}
					aria-hidden={true}
				>
					<path d="M3 11l9-7 9 7" />
					<path d="M5 10v10h14V10" />
					<path d="M10 20v-6h4v6" />
				</svg>
				<b style={{ color: 'var(--achievement)', fontWeight: 600, letterSpacing: '0.1em' }}>Obi</b>{' '}
				· the hearth
			</div>
			<h1>
				{tod.igbo}, <em>{name}</em>.
			</h1>
			<div className="gloss">
				<span className="english">{tod.english}</span>
				<span className="sep">·</span>
				<span>
					{now.toLocaleDateString(undefined, { weekday: 'short', month: 'short', day: 'numeric' })}
				</span>
				<span className="sep">·</span>
				<span>{now.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })}</span>
				<span className="sep">·</span>
			</div>
			<div className="shape">{shape}</div>
			{q && (
				<div className="quote">
					<div className="quote-body">
						&ldquo;{q.text}&rdquo;
						{q.gloss && <span className="quote-gloss">{q.gloss}</span>}
					</div>
					<div className="quote-attr">
						— {q.source}
						{q.work ? ` · ${q.work}` : ''}
					</div>
				</div>
			)}
		</>
	);
}

const QROW_STYLE: CSSProperties = {
	display: 'grid',
	gridTemplateColumns: '1fr auto auto',
	gap: 12,
	alignItems: 'center',
	padding: '7px 0',
	background: 'none',
	border: 0,
	width: '100%',
	textAlign: 'left',
	cursor: 'pointer',
	color: 'inherit',
	font: 'inherit',
};
const QMETA_STYLE: CSSProperties = {
	color: 'var(--fg-faint)',
	fontFamily: 'var(--font-mono, ui-monospace)',
	fontSize: 10.5,
};
const KBD_STYLE: CSSProperties = {
	display: 'inline-block',
	padding: '2px 6px',
	background: 'var(--bg-raised)',
	border: '1px solid var(--border)',
	borderRadius: 3,
	color: 'var(--fg-muted)',
	fontFamily: 'var(--font-mono, ui-monospace)',
	fontSize: 10,
};

/** Fixed shell verbs that are always dispatchable — no fetch, so no
 *  loading/error state applies to this half of the list (mirrors the design's
 *  "Quick actions has no error state — local in-memory registry" note). Real
 *  navigation, not decorative: each `go` is `usePaneStore`'s `navigateFocused`. */
function shellQuickActions(go: (to: string) => void): Array<{
	key: string;
	label: string;
	meta: string;
	keybind: string;
	onSelect: () => void;
}> {
	return [
		{
			key: 'new-session',
			label: 'New Claude session',
			meta: 'start',
			keybind: '⌘ N',
			onSelect: () => go('/sessions?new=1'),
		},
		{
			key: 'open-scratchpad',
			label: 'Open scratchpad',
			meta: 'start',
			keybind: '⌘ ⇧ N',
			onSelect: () => go('/scratchpads'),
		},
	];
}

/**
 * Quick actions — real commands, not a mock list. The always-available shell
 * verbs (new session / scratchpad) are local and instant; the rest are every
 * installed skill's dispatchable actions via the same `list_all_skill_actions`
 * query the ⌘K palette's `ActionsGroup` uses (`useAllSkillActions`,
 * `src/shell/palette-actions.tsx`). No `command_usage` table exists yet (per
 * shell memory / the design's honesty notes), so there is no real "recent" —
 * the shell verbs are simply always first, and skill actions follow in
 * whatever order the host returns them.
 */
function QuickBody() {
	const navigateFocused = usePaneStore((s) => s.navigateFocused);
	const { data, isLoading, isError, refetch } = useAllSkillActions();
	const shellActions = shellQuickActions((to) => navigateFocused(to));
	const skillActions = (data ?? []).filter(isDispatchable).slice(0, 4);

	return (
		<>
			{shellActions.map((a) => (
				<button key={a.key} type="button" style={QROW_STYLE} onClick={a.onSelect}>
					<span>{a.label}</span>
					<span style={QMETA_STYLE}>{a.meta}</span>
					<span style={KBD_STYLE}>{a.keybind}</span>
				</button>
			))}
			{isLoading && <SkeletonRows count={2} />}
			{isError && (
				<div style={{ ...QMETA_STYLE, padding: '6px 0' }}>
					Skill actions unavailable.{' '}
					<button
						type="button"
						onClick={() => void refetch()}
						style={{
							...QMETA_STYLE,
							textDecoration: 'underline',
							cursor: 'pointer',
							border: 0,
							background: 'none',
							padding: 0,
						}}
					>
						Retry
					</button>
				</div>
			)}
			{!isLoading &&
				!isError &&
				skillActions.map((action: SkillAction) => (
					<button
						key={`${action.pkgId}::${action.skill}/${action.verb}`}
						type="button"
						style={QROW_STYLE}
						onClick={() => void dispatchAction(action)}
					>
						<span>{action.name}</span>
						<span style={QMETA_STYLE}>skill</span>
						<span style={KBD_STYLE}>↵</span>
					</button>
				))}
		</>
	);
}

function PadBody() {
	// Lightweight: list scratchpads, render preview of the most-recent entry.
	// Avoids a second read-by-name fetch — `preview` is exactly what we want
	// on the home tile (full body lives at `/scratchpads`).
	const { data, isLoading } = useQuery({
		queryKey: ['home', 'scratchpads'],
		queryFn: () => listScratchpads(),
		staleTime: 30_000,
		refetchOnWindowFocus: true,
	});
	const first = data?.scratchpads?.[0];
	if (isLoading && !first) {
		return (
			<div
				style={{ color: 'var(--fg-faint)', fontSize: 12, padding: '20px 0', textAlign: 'center' }}
			>
				Loading…
			</div>
		);
	}
	if (!first) {
		return (
			<div
				style={{ color: 'var(--fg-faint)', fontSize: 12, padding: '20px 0', textAlign: 'center' }}
			>
				No scratchpads yet.
			</div>
		);
	}
	const edited = new Date(first.updated_at).toLocaleString(undefined, {
		hour: '2-digit',
		minute: '2-digit',
		month: 'short',
		day: 'numeric',
	});
	return (
		<>
			&ldquo;{first.preview || first.name}&rdquo;
			<div className="pad-meta">last edited · {edited}</div>
		</>
	);
}

/** "today" / "overdue" / weekday-short from a `due_date` (date-only string).
 *  Real tasks carry a date, not the mock's hour-granular "2h"/"EOD" labels —
 *  this is the honest downgrade from that fixture precision. */
function taskDueLabel(due: string, now: Date): { text: string; overdue: boolean } {
	const d = new Date(due);
	if (Number.isNaN(d.getTime())) return { text: due, overdue: false };
	const startOfToday = new Date(now.getFullYear(), now.getMonth(), now.getDate());
	const diffDays = Math.round((d.getTime() - startOfToday.getTime()) / 86_400_000);
	if (diffDays < 0) return { text: 'overdue', overdue: true };
	if (diffDays === 0) return { text: 'today', overdue: false };
	return { text: d.toLocaleDateString(undefined, { weekday: 'short' }), overdue: false };
}

function TasksBody() {
	const { data, isLoading, isError, refetch } = useQuery(homeTasksQueryOptions());
	if (isLoading) return <SkeletonRows />;
	if (isError) {
		return (
			<WidgetError
				title="Tasks unavailable"
				detail="Query to tasks failed — pkg may be uninstalled or DB locked."
				onRetry={() => void refetch()}
			/>
		);
	}
	const tasks = data ?? [];
	if (tasks.length === 0) {
		return <CenterNote>Nothing due today. A clear board.</CenterNote>;
	}
	const now = new Date();
	return (
		<>
			{tasks.map((t: HomeTaskRow) => {
				const blocked = t.status === 'blocked';
				const cls = blocked ? 'danger' : t.priority === 'high' ? 'warm' : '';
				const label = blocked
					? 'Blocked'
					: t.priority === 'high'
						? 'High priority'
						: 'Normal priority';
				const due = t.due_date ? taskDueLabel(t.due_date, now) : null;
				return (
					<div className="w-row" key={t.id}>
						<DotSpan cls={cls} label={label} />
						<span className="w-label">{t.title}</span>
						<span
							className="w-meta"
							style={blocked || due?.overdue ? { color: 'var(--danger)' } : undefined}
						>
							{blocked ? 'blocked' : (due?.text ?? '')}
						</span>
					</div>
				);
			})}
		</>
	);
}

/** Time label matching the design's "10:14 today / Mon earlier" split. */
function inboxTimeLabel(receivedAt: string, now: Date): string {
	const d = new Date(receivedAt);
	if (Number.isNaN(d.getTime())) return '';
	const sameDay = d.toDateString() === now.toDateString();
	if (sameDay) return d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
	const diffDays = Math.round((now.getTime() - d.getTime()) / 86_400_000);
	if (diffDays < 7) return d.toLocaleDateString(undefined, { weekday: 'short' });
	return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}

/** `from_address` is the only sender field the table carries (no display
 *  name column) — the local-part before `@` stands in for it, matching the
 *  design's "From · subject" shape without inventing a name. */
function fromLabel(fromAddress: string): string {
	const at = fromAddress.indexOf('@');
	return at > 0 ? fromAddress.slice(0, at) : fromAddress;
}

/** `triage_category` is free-form agent-assigned text (no documented enum) —
 *  best-effort mapping to a dot state. Anything else falls through to the
 *  default (uncoloured) dot rather than guessing. */
function inboxDotCls(category: string | null): { cls: string; label: string } {
	if (category === 'urgent') return { cls: 'danger', label: 'Urgent' };
	if (category === 'needs_reply' || category === 'warn')
		return { cls: 'warm', label: 'Needs reply' };
	return { cls: '', label: 'Info' };
}

function InboxBody() {
	const { data, isLoading, isError, refetch } = useQuery(homeInboxQueryOptions());
	if (isLoading) return <SkeletonRows />;
	if (isError) {
		return (
			<WidgetError
				title="Mail pkg unreachable"
				detail="Read of email_messages timed out. Last sync unknown."
				onRetry={() => void refetch()}
			/>
		);
	}
	const threads = data ?? [];
	if (threads.length === 0) {
		return <CenterNote>Inbox clear. No unread threads.</CenterNote>;
	}
	const now = new Date();
	return (
		<>
			{threads.map((m: HomeInboxRow) => {
				const { cls, label } = inboxDotCls(m.triage_category);
				return (
					<div className="w-row" key={m.id}>
						<DotSpan cls={cls} label={label} />
						<span className="w-label">
							{fromLabel(m.from_address)} · {m.subject || '(no subject)'}
						</span>
						<span className="w-meta">{inboxTimeLabel(m.received_at, now)}</span>
					</div>
				);
			})}
		</>
	);
}

/**
 * Boards — the least-grounded widget (design honesty notes: the studio pkg's
 * board/frame query surface isn't finalized; open question for the founder —
 * studio storyboards vs strategy-cycle boards). No boards/frames table exists
 * in `src-tauri/migrations/` to query, so this can only check whether
 * `com.ikenga.studio` is installed (a real, live signal) and render the
 * honest states around that absence rather than fabricate board rows. That
 * open question stays open — deferred, not resolved here.
 */
function BoardsBody() {
	const {
		data: installed,
		isLoading,
		isError,
		refetch,
	} = useQuery(homeBoardsPkgStatusQueryOptions());
	if (isLoading) return <SkeletonRows count={2} />;
	if (isError) {
		return (
			<WidgetError
				title="Pkg status unavailable"
				detail="Could not read the pkg kernel to check for com.ikenga.studio."
				onRetry={() => void refetch()}
			/>
		);
	}
	if (!installed) {
		return (
			<div style={{ ...CENTER_NOTE_STYLE, fontStyle: 'italic' }}>
				Studio pkg not installed.
				<br />
				This widget lights up once it's installed — board/frame query surface still TBD (design open
				question).
			</div>
		);
	}
	// Installed, but there is no boards/frames query surface yet to populate
	// this with real rows — see the module doc comment above.
	return <CenterNote>No active boards. Start one in Studio.</CenterNote>;
}

const SEVERITY_DOT: Record<string, string> = { crit: 'danger', warn: 'warm' };

/**
 * Finance — re-grounded to runway + top open alert per the design (replacing
 * the shipped WTD-revenue-sparkline mock, which `apps/finance` doesn't
 * actually compute). Cash is USD-only (no FX conversion — the local
 * `latest_account_balances` view has no cross-currency total, and inventing
 * one risks showing a wrong number for real money); burn is a trailing-30-day
 * net-outflow estimate; the 12-month target is a placeholder constant (no
 * config table carries a real one yet). The design's conservative/optimistic
 * scenario bands are DEFERRED — there's no burn-variance model in any local
 * table to derive them from honestly; this legend row shows the real cash +
 * burn figures instead. See the WP-18c report.
 */
function FinanceBody() {
	const { data, isLoading, isError, refetch } = useQuery(homeFinanceQueryOptions());
	if (isLoading) return <SkeletonRows count={1} />;
	if (isError) {
		return (
			<WidgetError
				title="Finance pkg unreachable"
				detail="finance_alerts / latest_account_balances read failed."
				onRetry={() => void refetch()}
			/>
		);
	}
	if (!data || !data.hasAnyData) {
		return <CenterNote>No finance data yet.</CenterNote>;
	}
	const { cashUsd, burnUsd30d, runwayMonths, topAlert } = data;
	const dotCls = topAlert ? (SEVERITY_DOT[topAlert.severity] ?? '') : 'live';
	return (
		<>
			<div
				style={{
					fontSize: 11,
					color: 'var(--fg-faint)',
					letterSpacing: '0.04em',
					fontFamily: 'var(--font-mono, ui-monospace)',
				}}
			>
				Runway · trailing 30d burn
			</div>
			<div className="figure">
				{runwayMonths != null ? (
					<>
						{runwayMonths.toFixed(1)}
						<span style={{ fontSize: 16, color: 'var(--fg-muted)', marginLeft: 2 }}>mo</span>{' '}
						<em
							style={runwayMonths < RUNWAY_TARGET_MONTHS ? { color: 'var(--danger)' } : undefined}
						>
							of {RUNWAY_TARGET_MONTHS} target
						</em>
					</>
				) : (
					<span style={{ fontSize: 20 }}>—</span>
				)}
			</div>
			{runwayMonths != null && (
				<div
					style={{
						height: 6,
						borderRadius: 999,
						background: 'var(--bg-sunken)',
						border: '1px solid var(--border-soft)',
						overflow: 'hidden',
					}}
				>
					<div
						style={{
							height: '100%',
							borderRadius: 999,
							width: `${Math.min(100, (runwayMonths / RUNWAY_TARGET_MONTHS) * 100)}%`,
							background: 'linear-gradient(90deg, var(--danger), var(--achievement))',
						}}
					/>
				</div>
			)}
			{(cashUsd != null || burnUsd30d != null) && (
				<div
					style={{
						display: 'flex',
						justifyContent: 'space-between',
						marginTop: 5,
						fontFamily: 'var(--font-mono, ui-monospace)',
						fontSize: 10,
						color: 'var(--fg-faint)',
					}}
				>
					<span>{cashUsd != null ? `cash $${(cashUsd / 1000).toFixed(1)}k` : 'cash —'}</span>
					<span>
						{burnUsd30d != null ? `burn $${(burnUsd30d / 1000).toFixed(1)}k/mo` : 'burn —'}
					</span>
				</div>
			)}
			<div
				style={{
					display: 'flex',
					alignItems: 'flex-start',
					gap: 8,
					marginTop: 12,
					paddingTop: 8,
					borderTop: '1px solid var(--border-soft)',
					fontSize: 12,
					color: 'var(--fg-muted)',
				}}
			>
				<span style={{ marginTop: 3 }}>
					<DotSpan cls={dotCls} label={topAlert ? topAlert.severity : 'Clear'} />
				</span>
				<span>{topAlert ? topAlert.message : 'All clear — no open alerts.'}</span>
			</div>
		</>
	);
}

function widgetMeta(kind: WidgetKind): {
	title: string;
	icon: ReactNode;
	tag: ReactNode;
	Body: () => ReactNode;
	cls?: string;
} | null {
	switch (kind) {
		case 'quick':
			return { title: 'Quick actions', icon: Icons.quick, tag: '⌘ K', Body: QuickBody };
		case 'pad':
			return { title: 'Scratchpad', icon: Icons.pad, tag: 'auto', Body: PadBody, cls: 'w-pad' };
		case 'tasks':
			return { title: "Today's tasks", icon: Icons.tasks, tag: 'pkg · tasks', Body: TasksBody };
		case 'inbox':
			return { title: 'Inbox triage', icon: Icons.mail, tag: 'pkg · email', Body: InboxBody };
		case 'boards':
			return { title: 'Active boards', icon: Icons.studio, tag: 'pkg · studio', Body: BoardsBody };
		case 'finance':
			return {
				title: 'Week so far',
				icon: Icons.finance,
				tag: 'pkg · finance',
				Body: FinanceBody,
				cls: 'w-finance',
			};
		default:
			return null;
	}
}

// ───────────────────────── canvas ─────────────────────────
// The free-form pan/zoom/grid-snap surface now lives in the reusable
// <Canvas> primitive (`@ikenga/contract/canvas`). Home stays the source of
// truth for `layout` (+ localStorage persistence), `editMode`, `selectedId`,
// and the viewport; Canvas drives the gestures and calls back on change.
//
// Two adapters bridge home's legacy `WidgetPlacement[]` (ordered, kind-tagged)
// to the canvas's `Record<ItemId, Placement>`: `toRecord` strips to geometry,
// `applyRecord` writes moved coordinates back onto the ordered array so order
// + kind + patina survive the round-trip.

function toRecord(layout: WidgetPlacement[]): Record<ItemId, Placement> {
	const rec: Record<ItemId, Placement> = {};
	for (const w of layout) {
		rec[w.id as ItemId] = { x: w.x, y: w.y, w: w.w, h: w.h };
	}
	return rec;
}

function applyRecord(layout: WidgetPlacement[], rec: Record<ItemId, Placement>): WidgetPlacement[] {
	return layout.map((w) => {
		const p = rec[w.id as ItemId];
		return p ? { ...w, x: p.x, y: p.y, w: p.w, h: p.h } : w;
	});
}

/** `hideGreeting`: the D-04 daily address above the canvas is the greeting
 *  while it shows, so the canvas's own greeting widget steps aside. */
export function Home({ hideGreeting = false }: { hideGreeting?: boolean } = {}) {
	// Acknowledge partOfDay so its import survives tree-shaking under noUnused.
	void partOfDay;

	const [layout, setLayout] = useState<WidgetPlacement[]>(() => {
		try {
			const raw = localStorage.getItem(LS_LAYOUT);
			if (raw) {
				const parsed = JSON.parse(raw);
				if (Array.isArray(parsed) && parsed.length) return parsed;
			}
		} catch {
			// ignore
		}
		return DEFAULT_LAYOUT;
	});
	useEffect(() => {
		try {
			localStorage.setItem(LS_LAYOUT, JSON.stringify(layout));
		} catch {
			// ignore
		}
	}, [layout]);

	const [editMode, setEditMode] = useState(false);
	const [selectedId, setSelectedId] = useState<ItemId | null>(null);
	const [viewport, setViewport] = useState<Viewport>({ x: 0, y: 0, scale: 1 });

	const canvasHandle = useRef<CanvasHandle>(null);

	const layoutRecord = useMemo(() => toRecord(layout), [layout]);

	// renderItem owns the widgetMeta lookup + Body invocation + greeting
	// special-case. It returns the consumer's positioned element (home-greeting
	// or home-widget) carrying its base class + height; Canvas injects data-id,
	// left/top/width geometry, and the is-selected class.
	function renderItem(w: WidgetPlacement, state: ItemRenderState): ReactNode {
		if (w.kind === 'greeting') {
			return (
				<div className="home-greeting" style={{ minHeight: state.placement.h }}>
					<GreetingBody name="friend" />
				</div>
			);
		}
		const m = widgetMeta(w.kind);
		if (!m) return null;
		const Body = m.Body;
		return (
			<div
				className={`home-widget ${m.cls ?? ''}`.trim()}
				data-patina={w.patina ?? undefined}
				style={{ height: state.placement.h }}
			>
				<div className="w-head">
					<div className="w-icon">{m.icon}</div>
					<div className="w-title">{m.title}</div>
					<div className="w-tag">{m.tag}</div>
				</div>
				<div className="w-body">
					<Body />
				</div>
			</div>
		);
	}

	return (
		<Canvas<WidgetPlacement>
			ref={canvasHandle}
			items={hideGreeting ? layout.filter((w) => w.kind !== 'greeting') : layout}
			itemId={(w) => w.id as ItemId}
			itemKind={(w) => w.kind}
			layout={layoutRecord}
			viewport={viewport}
			editMode={editMode}
			selectedId={selectedId}
			gridSnap={12}
			ariaLabel="Obi home canvas"
			renderItem={renderItem}
			onLayoutChange={(rec) => setLayout((L) => applyRecord(L, rec))}
			onViewportChange={setViewport}
			onEditModeChange={setEditMode}
			onSelectionChange={setSelectedId}
		>
			<div className="ikenga-canvas-bar">
				<div className="crumb">
					App · <b>Obi</b>
				</div>
				<div className="hint">
					<span className="kbd">space</span> drag · <span className="kbd">↑↓←→</span> pan ·{' '}
					<span className="kbd">+/-</span> zoom · <span className="kbd">dbl-click</span> re-fit
				</div>
				<button className="btn" type="button" onClick={() => canvasHandle.current?.autoFit(true)}>
					{Icons.recenter}
					Recenter
				</button>
				<button
					className={`btn${editMode ? ' is-primary' : ''}`}
					type="button"
					aria-pressed={editMode}
					onClick={() => {
						setEditMode((v) => !v);
						setSelectedId(null);
					}}
				>
					{Icons.edit}
					{editMode ? 'Done' : 'Customize'}
				</button>
			</div>

			<aside
				className="home-palette"
				aria-label="Widget palette"
				aria-hidden={!editMode}
				onClick={(e) => e.stopPropagation()}
			>
				<div className="head">
					<div className="ptag">Customize</div>
					<h3>Widget palette</h3>
					<p>Drag any item onto the canvas. Placed widgets are dimmed.</p>
				</div>
				<div className="section">
					<h4>Shell · {PALETTE_SHELL.length} widgets</h4>
					<div className="list">
						{PALETTE_SHELL.map((p) => {
							const placed = layout.some((l) => l.id === p.id);
							return (
								<div key={p.id} className={`item${placed ? ' is-placed' : ''}`}>
									<div className="pic">{p.icon}</div>
									<div className="title">{p.title}</div>
									<span className="meta">{placed ? 'placed' : 'drag'}</span>
								</div>
							);
						})}
					</div>
				</div>
				<div className="section">
					<h4>Pkgs · {PALETTE_PKG.length} available</h4>
					<div className="list">
						{PALETTE_PKG.map((p) => {
							const placed = layout.some((l) => l.id === p.id);
							return (
								<div key={p.id} className={`item${placed ? ' is-placed' : ''}`}>
									<div className="pic">{p.icon}</div>
									<div className="title">{p.title}</div>
									<span className="meta">pkg · {p.meta}</span>
								</div>
							);
						})}
					</div>
				</div>
			</aside>
		</Canvas>
	);
}
