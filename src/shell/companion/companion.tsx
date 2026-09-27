// The Chi Companion — the right-hand region of the locked frame (D-01,
// `designs/frame-workbench-v4.html`; spec §3.9–§3.12, §5). Was the Dock.
//
// It renders STATE, never model prose (ADR-021 / spec §5.4): a dispatch bar
// (the one dispatch input), the seat rail (WP-67, D-09 `seats-companion.html`
// — the session tabs became seats), and panels — permissions, cost, tool
// feed, runs — scoped to the selected seat. Whatever an agent says lives in
// the terminal pane, never here. `companion.conformance.test.ts` enforces
// that for everything under this directory.
//
// D-09 state map (G-55 `data-state`): `seats-roster` / `seats-empty` (the
// rail), `seats-create` (the New-seat form), `seats-vacant` (a vacant seat's
// panel), `seats-dispatch` (the target picker open), `seats-rest` (the 36 px
// strip). `popout` is WP-69's.

import { ChevronRight, ShieldCheck } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { cn } from '@/components/ui/utils';
import { EmptyState } from '@/components/states';
import { LoreTerm } from '@/components/lore/lore-term';
import { focusMarkerProps } from '@/lib/keymap/context-keys';
import { useCommands } from '@/lib/keymap/dispatcher';
import { findEntry, labelFor } from '@/lib/keymap/registry';
import { useDragState } from '@/lib/panes/drag-state';
import { usePaneStore } from '@/lib/panes/pane-store';
import { useDropTarget } from '@/lib/panes/pointer-drag';
import type { LeafNode, PaneNode, PaneView } from '@/lib/panes/types';
import { queryClient } from '@/lib/query-client';
import { useShellStore } from '@/lib/shell/shell-store';
import type { DetectedAgent, SeatView } from '@/lib/tauri-cmd';
import { COMPANION_FOCUS_EVENT } from '@/shell/companion-focus';
import { listen } from '@/lib/transport';
import { CostHud } from '@/terminal/cost-hud';
import { MissionControl } from '@/terminal/mission-control';
import { ToolCallFeed } from '@/terminal/tool-call-feed';
import { CollapsedStrip } from './collapsed-strip';
import {
	COMPANION_MAX_WIDTH,
	COMPANION_MIN_WIDTH,
	focusCompanion,
	type PermissionCardEntry,
	type PermissionDecision,
	useCompanionStore,
} from './companion-store';
import { DispatchBar } from './dispatch-bar';
import { applyTarget, cycleIn, selectSeat, selectSession, useSeatUi } from './seat-actions';
import { SeatForm } from './seat-form';
import { atName, costLine, seatSessionRef, UNREPORTED } from './seat-model';
import { SeatNoticeHost } from './seat-notice-host';
import { SeatRail, useRailSync } from './seat-rail';
import { SeatRemoveDialog } from './seat-remove-dialog';
import { type SeatRoster, useSeatRoster } from './seat-roster';
import { sessionName, useSessionFigures } from './seat-sessions';
import { SeatVacantPanel } from './seat-vacant-panel';
import { cycleTargets, DETECT_AGENTS_KEY, offeredEngines, useTargetLabel } from './target-picker';

/** Window event WP-03's ⌘2 (rail → Chi) dispatches — owned by the rail's
 *  dependency-free seam module; re-exported here for tests and callers. */
export { COMPANION_FOCUS_EVENT };
/** Window event the scoped panels' empty action dispatches. */
export const COMPANION_PICK_SESSION_EVENT = 'ikenga:companion-pick-session';

type HookPayload = {
	request_id?: string;
	ikenga_terminal_id?: string;
	hook_event_name?: string;
	tool_name?: string;
	tool_input?: Record<string, unknown>;
	prompt?: string;
	held?: boolean;
};

/** Feeds the §5.6 permission queue from the hooks bus. Mounted once, by the
 *  Companion, whatever its state — the strip badge needs the count too. */
function usePermissionFeed() {
	useEffect(() => {
		const offs: Array<() => void> = [];
		let alive = true;
		const keep = (p: Promise<() => void>) =>
			p.then((off) => (alive ? offs.push(off) : off())).catch(() => {});
		keep(
			listen<HookPayload>('hooks://event', (event) => {
				const p = event.payload;
				if (!p?.request_id) return;
				const isPermission = p.hook_event_name === 'PermissionRequest';
				const isHeldTool = p.hook_event_name === 'PreToolUse' && p.held;
				if (!isPermission && !isHeldTool) return;
				useCompanionStore.getState().receivePermission({
					id: p.request_id,
					kind: isPermission ? 'permission' : 'tool_use',
					toolName: p.tool_name || (isPermission ? 'Action' : 'Tool use'),
					toolInput: p.tool_input,
					prompt: p.prompt,
					sessionId: p.ikenga_terminal_id,
				});
			})
		);
		keep(
			listen<{ requestId?: string; decision?: string }>('hooks://decision', (event) => {
				const d = event.payload;
				if (!d?.requestId) return;
				useCompanionStore.getState().permissionDecided(d.requestId, d.decision ?? 'denied');
			})
		);
		return () => {
			alive = false;
			for (const off of offs) off();
		};
	}, []);
}

/** ⌘2 (`ikenga:companion-focus`) and the panels' "Choose a session" action. */
function useCompanionWindowEvents() {
	useEffect(() => {
		const onFocus = () => focusCompanion();
		const onPick = () => useCompanionStore.getState().openTargetPicker();
		window.addEventListener(COMPANION_FOCUS_EVENT, onFocus);
		window.addEventListener(COMPANION_PICK_SESSION_EVENT, onPick);
		return () => {
			window.removeEventListener(COMPANION_FOCUS_EVENT, onFocus);
			window.removeEventListener(COMPANION_PICK_SESSION_EVENT, onPick);
		};
	}, []);
}

function findLeaf(node: PaneNode, id: string): LeafNode | null {
	if (node.type === 'leaf') return node.id === id ? node : null;
	for (const child of node.children) {
		const found = findLeaf(child, id);
		if (found) return found;
	}
	return null;
}

/** The pane tab a pane-source drag is carrying, if any. */
function draggedPaneView(): PaneView | null {
	const d = useDragState.getState();
	if (!d.active || d.source !== 'pane' || d.srcLeafId == null || d.srcTabIdx == null) return null;
	return findLeaf(usePaneStore.getState().root, d.srcLeafId)?.tabs[d.srcTabIdx] ?? null;
}

/** Where a permission card lands after a monogram click (D-09: "clicking
 *  the lead monogram lands on its permission card"). */
function focusPermissionFor(sessionId: string | null): void {
	setTimeout(() => {
		const id =
			sessionId && typeof CSS !== 'undefined' && typeof CSS.escape === 'function'
				? CSS.escape(sessionId)
				: (sessionId ?? '').replace(/["\\]/g, '\\$&');
		const sel = sessionId
			? `[data-permission-card][data-permission-session="${id}"]`
			: '[data-permission-card][data-status="pending"]';
		document.querySelector<HTMLElement>(sel)?.focus();
	}, 0);
}

/** ⌥↑ / ⌥↓ — cycle the dispatch target (D-09 rule 5). Fires anywhere in the
 *  Companion, including the dispatch input, but never from another text
 *  field (the New-seat name, a rename). Scoped to the Companion: a global
 *  binding would be a `defaults.ts` entry, which is WP-68's file. */
function onCycleKey(e: React.KeyboardEvent, roster: SeatRoster): void {
	if (!e.altKey || e.metaKey || e.ctrlKey || (e.key !== 'ArrowUp' && e.key !== 'ArrowDown')) return;
	const el = e.target as HTMLElement;
	const isField = el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable;
	if (isField && !el.hasAttribute('data-companion-dispatch')) return;
	e.preventDefault();
	const shell = useShellStore.getState();
	const engines = offeredEngines(
		shell.defaultEngineId,
		queryClient.getQueryData<DetectedAgent[]>([...DETECT_AGENTS_KEY])
	);
	const next = cycleIn(
		cycleTargets(roster.seats, roster.unseated, engines),
		shell.companion.activeTarget,
		e.key === 'ArrowDown' ? 1 : -1
	);
	if (next) applyTarget(next);
}

export function Companion() {
	const companionState = useCompanionStore((s) => s.state);
	const setState = useCompanionStore((s) => s.setState);
	const appendView = useCompanionStore((s) => s.appendView);
	const width = useCompanionStore((s) => s.width);
	const setWidth = useCompanionStore((s) => s.setWidth);
	const pending = useCompanionStore(
		(s) => s.permissions.filter((p) => p.status === 'pending').length
	);
	const sel = useCompanionStore((s) => s.railSelection);
	const form = useSeatUi((s) => s.form);
	const roster = useSeatRoster();
	const live =
		roster.seats.some((s) => s.status === 'live' || s.status === 'run') ||
		roster.unseated.some((u) => u.status === 'running');
	const [dropHover, setDropHover] = useState(false);

	usePermissionFeed();
	useCompanionWindowEvents();
	useRailSync(roster);

	// Pane → Companion. A session is a reference, not a move: the terminal
	// stays in its pane (output lives there, ADR-021) and the Companion gains
	// it as a rail row (unseated, or its seat). Only terminal tabs are sessions.
	const drop = useDropTarget({
		accepts: () => draggedPaneView()?.kind === 'terminal',
		onOver: () => setDropHover(true),
		onLeave: () => setDropHover(false),
		onDrop: () => {
			setDropHover(false);
			const view = draggedPaneView();
			if (view?.kind === 'terminal') appendView({ kind: 'terminal', sessionId: view.sessionId });
			useDragState.getState().end();
		},
	});

	if (companionState === 'hidden') return <SeatNoticeHost />;

	const selectedKey = sel ? (sel.kind === 'seat' ? `seat:${sel.seat_id}` : `session:${sel.session_id}`) : null;

	if (companionState === 'collapsed') {
		return (
			<>
				<CollapsedStrip
					pendingPermissions={pending}
					live={live}
					onExpand={() => useCompanionStore.getState().focusDispatch()}
					seats={roster.seats}
					unseated={roster.unseated}
					pendingBySession={roster.pendingBySession}
					pendingUnattributed={roster.pendingUnattributed}
					selectedKey={selectedKey}
					onPickSeat={(seat, toPermission) => {
						setState('expanded');
						selectSeat(seat);
						if (toPermission) focusPermissionFor(seatSessionRef(seat));
					}}
					onPickSession={(id, toPermission) => {
						setState('expanded');
						selectSession(id);
						if (toPermission) focusPermissionFor(id);
					}}
					dropProps={drop}
					dropHover={dropHover}
				/>
				<SeatNoticeHost />
				<SeatRemoveDialog roster={roster} />
			</>
		);
	}

	return (
		<aside
			aria-label="Chi companion"
			className={cn(
				'relative flex h-full shrink-0 flex-col border-l',
				dropHover && 'ring-2 ring-inset ring-[var(--primary)]'
			)}
			style={{
				width: `${width}px`,
				background: 'var(--bg-surface)',
				borderColor: 'var(--border-soft)',
			}}
			onKeyDown={(e) => onCycleKey(e, roster)}
			{...drop}
		>
			<CompanionResizeHandle width={width} setWidth={setWidth} />
			<CompanionHeader onCollapse={() => setState('collapsed')} roster={roster} />
			<DispatchBar roster={roster} />
			{form ? (
				<SeatForm key={form.seatSession ?? 'new'} roster={roster} init={form} />
			) : (
				<>
					<SeatRail roster={roster} />
					<CompanionPanels roster={roster} />
				</>
			)}
			<SeatNoticeHost />
			<SeatRemoveDialog roster={roster} />
		</aside>
	);
}

function CompanionHeader({ onCollapse, roster }: { onCollapse: () => void; roster: SeatRoster }) {
	const target = useShellStore((s) => s.companion.activeTarget);
	const sel = useCompanionStore((s) => s.railSelection);
	const targetLabel = useTargetLabel(target);
	// D-09: the header names the selected seat (`@lead`) — whose state the
	// panels below are reading.
	const selSeat = sel?.kind === 'seat' ? roster.seats.find((s) => s.id === sel.seat_id) : undefined;
	const caption = selSeat
		? atName(selSeat.name)
		: sel?.kind === 'session'
			? sessionName(sel.session_id)
			: targetLabel;
	// ⌘2 is WP-03's `rail.chi`; the hint appears once that binding exists.
	const chiKey = findEntry('rail.chi') ? labelFor('rail.chi') : '';
	const toggleKey = labelFor('companion.toggle');
	return (
		<div
			className="flex h-9 shrink-0 items-center gap-2 border-b px-3"
			style={{ borderColor: 'var(--border)' }}
		>
			<span className="text-xs font-semibold" style={{ color: 'var(--fg)' }}>
				<LoreTerm term="Chi" />
			</span>
			<button
				type="button"
				onClick={() =>
					document
						.querySelector<HTMLElement>(
							'[role="listbox"][aria-label="Seats and unseated sessions"] [aria-selected="true"]'
						)
						?.focus()
				}
				className="min-w-0 truncate rounded-sm font-mono text-[11px] text-[var(--fg-muted)] hover:text-[var(--fg)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
				title="Go to the selected seat"
			>
				{caption}
			</button>
			<span className="flex-1" />
			{chiKey && (
				<span className="font-mono text-[11px]" style={{ color: 'var(--fg-muted)' }}>
					{chiKey}
				</span>
			)}
			<button
				type="button"
				onClick={onCollapse}
				aria-expanded={true}
				aria-label="Collapse Companion"
				title={toggleKey ? `Collapse to strip (${toggleKey})` : 'Collapse to strip'}
				className="grid size-6 place-items-center rounded-sm text-[var(--fg-muted)] hover:bg-[var(--bg-raised)] hover:text-[var(--fg)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
			>
				<ChevronRight className="h-3.5 w-3.5" aria-hidden="true" />
			</button>
		</div>
	);
}

function Panel({
	title,
	scope,
	count,
	countTitle,
	defaultOpen = true,
	grow,
	children,
}: {
	title: string;
	/** The seat or session the panel is scoped to (`@lead`), D-09. */
	scope?: string | null;
	count?: string;
	countTitle?: string;
	defaultOpen?: boolean;
	grow?: boolean;
	children: React.ReactNode;
}) {
	const [open, setOpen] = useState(defaultOpen);
	return (
		<section
			aria-label={title}
			className={cn('border-b', grow && open ? 'flex min-h-40 flex-1 flex-col' : 'shrink-0')}
			style={{ borderColor: 'var(--border-soft)' }}
		>
			<button
				type="button"
				aria-expanded={open}
				onClick={() => setOpen((o) => !o)}
				className="flex h-7 w-full items-center gap-2 px-3 text-[var(--fg-muted)] hover:text-[var(--fg)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
			>
				<span className="text-[11px] font-semibold uppercase tracking-widest">{title}</span>
				{scope && (
					<span className="font-mono text-[11px]" style={{ color: 'var(--fg)' }}>
						{scope}
					</span>
				)}
				{count && (
					<span className="ml-auto font-mono text-[11px]" title={countTitle}>
						{count}
					</span>
				)}
			</button>
			{open && <div className={cn(grow && 'min-h-0 flex-1 overflow-hidden')}>{children}</div>}
		</section>
	);
}

/** Who a session belongs to, in the rail's words: `@lead`, or `session 4`. */
function ownerLabel(sessionId: string, seats: readonly SeatView[]): string {
	const seat = seats.find((s) => seatSessionRef(s) === sessionId);
	return seat ? atName(seat.name) : sessionName(sessionId);
}

function CompanionPanels({ roster }: { roster: SeatRoster }) {
	const scope = useCompanionStore((s) => s.panelScopeSessionId);
	const sel = useCompanionStore((s) => s.railSelection);
	const permissions = useCompanionStore((s) => s.permissions);
	const figures = useSessionFigures(scope);
	const selSeat = sel?.kind === 'seat' ? roster.seats.find((s) => s.id === sel.seat_id) : undefined;
	const scopeLabel = selSeat ? atName(selSeat.name) : scope ? sessionName(scope) : null;
	// G-SEATS §9.1: selecting a seat scopes the permission panel too. A card
	// with no session id can't be pinned on anyone, so it shows everywhere.
	const shown = scope ? permissions.filter((p) => !p.sessionId || p.sessionId === scope) : permissions;
	const pending = shown.filter((p) => p.status === 'pending').length;
	const elsewhere = new Map<string, number>();
	if (scope) {
		for (const p of permissions) {
			if (p.status !== 'pending' || !p.sessionId || p.sessionId === scope) continue;
			elsewhere.set(p.sessionId, (elsewhere.get(p.sessionId) ?? 0) + 1);
		}
	}
	const cost = scope ? costLine(figures) : null;
	// Keyed by scope: the HUDs subscribe once per mount, so a scope change
	// remounts them rather than reaching into their internals.
	const scopeKey = scope ?? 'none';
	return (
		<div className="flex min-h-0 flex-1 flex-col overflow-y-auto">
			{selSeat && selSeat.status === 'vacant' && <SeatVacantPanel seat={selSeat} />}
			<Panel title="Permissions" scope={scopeLabel} count={pending ? `${pending} pending` : 'none pending'}>
				{/* §5.6 cards only: the scoped PermissionInbox HUD stays in the
				    terminal host's side panels, not in the Companion. */}
				<PermissionCards cards={shown} ownerOf={(id) => ownerLabel(id, roster.seats)} />
				{elsewhere.size > 0 && (
					<div className="flex flex-wrap gap-2 px-3 pb-3">
						{[...elsewhere.entries()].map(([sessionId, n]) => (
							<button
								key={sessionId}
								type="button"
								data-permission-elsewhere={sessionId}
								onClick={() => {
									const seat = roster.seats.find((s) => seatSessionRef(s) === sessionId);
									if (seat) selectSeat(seat);
									else selectSession(sessionId);
									focusPermissionFor(sessionId);
								}}
								className="inline-flex h-5 items-center gap-1 rounded-[var(--radius-xs)] px-1 text-[11px] hover:bg-[var(--achievement-soft)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
								style={{ color: 'var(--on-achievement)' }}
							>
								{`${n} pending on ${ownerLabel(sessionId, roster.seats)}`}
							</button>
						))}
					</div>
				)}
			</Panel>
			<Panel
				title="Cost"
				scope={scopeLabel}
				count={cost?.text}
				countTitle={cost?.unreported ? UNREPORTED : undefined}
			>
				<CostHud key={scopeKey} sessionId={scope} />
			</Panel>
			<Panel title="Tool feed" scope={scopeLabel} grow>
				<ToolCallFeed key={scopeKey} sessionId={scope} />
			</Panel>
			{/* `embedded`: MissionControl drops its own dispatcher and sample
			    figures, so the dispatch bar stays the Companion's one input. */}
			<Panel title="Runs">
				<div className="max-h-72 overflow-hidden">
					<MissionControl embedded />
				</div>
			</Panel>
		</div>
	);
}

const DECISION_TEXT: Record<PermissionDecision, string> = {
	allow: 'Allowed once',
	always: 'Always allowed for this project',
	deny: 'Denied',
};

export function PermissionCards({
	cards,
	ownerOf,
}: {
	cards: PermissionCardEntry[];
	/** `@lead` / `session 4` for a card's session — the card says who asked. */
	ownerOf?: (sessionId: string) => string;
}) {
	const resolve = useCompanionStore((s) => s.resolvePermission);
	// A/D allow/deny the *focused* card (spec §2) — one pair of registry
	// commands for the whole list (WP-56, G-ACTIONS §10.2/§10.6), reading
	// which card holds focus at fire time rather than one handler per row
	// (`useCommands`'s per-command stack would only ever run the
	// last-mounted row's closure).
	const resolveFocused = (decision: 'allow' | 'deny') => {
		const el = document.activeElement;
		// Fix round 1: a focused button (Allow once / Always / Deny) handles
		// its own Enter/Space — A/D must not also act behind its back.
		if (el instanceof HTMLButtonElement) return;
		const id = el instanceof Element ? el.closest('[data-permission-card]')?.getAttribute('data-permission-card') : null;
		if (!id) return;
		const card = cards.find((c) => c.id === id);
		if (!card || card.status !== 'pending') return;
		resolve(id, decision);
	};
	useCommands({
		'companion.permission-allow': () => resolveFocused('allow'),
		'companion.permission-deny': () => resolveFocused('deny'),
	});

	if (cards.length === 0) {
		return (
			<EmptyState
				data-state="companion-no-permissions"
				icon={ShieldCheck}
				heading="Nothing waiting"
				body="Permission requests land here. An empty inbox is the ordinary state, not a problem."
				fill={false}
				className="min-h-0 gap-1.5 p-3 pb-2"
				action={{
					label: 'See what is allowed',
					onClick: () => usePaneStore.getState().navigateFocused('/packages'),
				}}
			/>
		);
	}
	return (
		<div className="flex flex-col gap-2 px-3 pb-3">
			{cards.map((card) => (
				<PermissionCard
					key={card.id}
					card={card}
					owner={card.sessionId && ownerOf ? ownerOf(card.sessionId) : null}
				/>
			))}
		</div>
	);
}

function PermissionCard({ card, owner }: { card: PermissionCardEntry; owner: string | null }) {
	const undo = useCompanionStore((s) => s.undoPermission);
	const resolve = useCompanionStore((s) => s.resolvePermission);

	const title = card.kind === 'tool_use' ? `Tool use: ${card.toolName}` : card.toolName;
	return (
		<fieldset
			// biome-ignore lint/a11y/noNoninteractiveTabindex: the card is the focus target for A / D (spec §2)
			tabIndex={0}
			aria-label={`Permission request: ${title}`}
			{...focusMarkerProps('permission-card')}
			data-permission-card={card.id}
			data-permission-session={card.sessionId}
			data-status={card.status}
			className={cn(
				'min-w-0 rounded-md border p-3 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring',
				card.status === 'resolved' && 'opacity-60'
			)}
			style={{ borderColor: 'var(--achievement-soft)', background: 'var(--bg-raised)' }}
		>
			{owner && (
				<div className="mb-1 text-[11px]" style={{ color: 'var(--fg-muted)' }}>
					from{' '}
					<span className="font-mono" style={{ color: 'var(--fg)' }}>
						{owner}
					</span>
				</div>
			)}
			<div className="text-[13px]" style={{ color: 'var(--fg)' }}>
				<span className="font-mono">{title}</span>
			</div>
			{card.prompt && (
				<p className="mt-1 text-xs" style={{ color: 'var(--fg-muted)' }}>
					{card.prompt}
				</p>
			)}
			{card.toolInput && (
				<pre
					// biome-ignore lint/a11y/noNoninteractiveTabindex: scrollable region must be keyboard-reachable
					tabIndex={0}
					className="mt-1.5 max-h-24 overflow-auto rounded p-2 font-mono text-[10px]"
					style={{ background: 'var(--bg-sunken)', color: 'var(--fg-muted)' }}
				>
					{JSON.stringify(card.toolInput, null, 2)}
				</pre>
			)}
			{card.status === 'pending' && (
				<div className="mt-3 flex flex-wrap gap-2">
					<CardButton tone="primary" onClick={() => resolve(card.id, 'allow')}>
						Allow once
					</CardButton>
					<CardButton tone="ghost" onClick={() => resolve(card.id, 'always')}>
						Always for this project
					</CardButton>
					<CardButton tone="danger" onClick={() => resolve(card.id, 'deny')}>
						Deny
					</CardButton>
				</div>
			)}
			{card.status === 'undoable' && card.decision && (
				<div role="status" className="mt-3 flex items-center gap-2 text-xs">
					<span style={{ color: 'var(--fg)' }}>{DECISION_TEXT[card.decision]}</span>
					<CardButton tone="ghost" onClick={() => undo(card.id)}>
						Undo
					</CardButton>
				</div>
			)}
			{card.status === 'resolved' && (
				<div className="mt-2 text-[11px]" style={{ color: 'var(--fg-muted)' }}>
					{card.appliedElsewhere
						? 'Already applied'
						: card.decision
							? DECISION_TEXT[card.decision]
							: 'Resolved'}
					{card.ruleFile && (
						<span className="block font-mono">Rule written to {card.ruleFile}</span>
					)}
					{card.error && (
						<span role="alert" className="block" style={{ color: 'var(--color-text-danger)' }}>
							{card.error}
						</span>
					)}
				</div>
			)}
		</fieldset>
	);
}

function CardButton({
	tone,
	onClick,
	children,
}: {
	tone: 'primary' | 'ghost' | 'danger';
	onClick: () => void;
	children: React.ReactNode;
}) {
	return (
		<button
			type="button"
			onClick={onClick}
			className={cn(
				'min-h-6 rounded-sm border px-2 text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring',
				tone === 'primary' &&
					'border-transparent bg-[var(--primary)] text-[var(--primary-fg)] hover:opacity-90',
				tone === 'ghost' && 'border-[var(--border)] text-[var(--fg)] hover:bg-[var(--bg-sunken)]',
				tone === 'danger' &&
					'border-[var(--color-border-danger)] text-[var(--color-text-danger)] hover:bg-[var(--danger-soft)]'
			)}
		>
			{children}
		</button>
	);
}

function CompanionResizeHandle({
	width,
	setWidth,
}: {
	width: number;
	setWidth: (n: number) => void;
}) {
	const startRef = useRef<{ x: number; w: number } | null>(null);

	function onPointerDown(e: React.PointerEvent<HTMLDivElement>) {
		e.preventDefault();
		(e.currentTarget as HTMLDivElement).setPointerCapture(e.pointerId);
		startRef.current = { x: e.clientX, w: width };
		document.body.style.cursor = 'col-resize';
		document.body.style.userSelect = 'none';
	}

	function onPointerMove(e: React.PointerEvent<HTMLDivElement>) {
		if (!startRef.current) return;
		// The Companion is on the right edge — dragging left grows it.
		const next = startRef.current.w - (e.clientX - startRef.current.x);
		setWidth(Math.max(COMPANION_MIN_WIDTH, Math.min(COMPANION_MAX_WIDTH, next)));
	}

	function onPointerUp(e: React.PointerEvent<HTMLDivElement>) {
		startRef.current = null;
		try {
			(e.currentTarget as HTMLDivElement).releasePointerCapture(e.pointerId);
		} catch {
			// ignore
		}
		document.body.style.cursor = '';
		document.body.style.userSelect = '';
	}

	// Keyboard alternative for the resize drag (←/→ 16 px).
	function onKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
		if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
		e.preventDefault();
		setWidth(width + (e.key === 'ArrowLeft' ? 16 : -16));
	}

	return (
		// biome-ignore lint/a11y/useSemanticElements: a focusable, draggable window splitter (WAI-ARIA separator pattern) — an <hr> is neither
		<div
			role="separator"
			aria-orientation="vertical"
			aria-label="Resize Companion"
			aria-valuenow={width}
			aria-valuemin={COMPANION_MIN_WIDTH}
			aria-valuemax={COMPANION_MAX_WIDTH}
			tabIndex={0}
			onPointerDown={onPointerDown}
			onPointerMove={onPointerMove}
			onPointerUp={onPointerUp}
			onPointerCancel={onPointerUp}
			onKeyDown={onKeyDown}
			className="absolute left-0 top-0 z-10 h-full w-1 -translate-x-1/2 cursor-col-resize hover:bg-[var(--primary-soft)] focus-visible:bg-[var(--primary)] focus-visible:outline-none"
		/>
	);
}
