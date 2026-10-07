// Target chip + picker — spec §3.10 #62, D-09 `dispatch` state (WP-67). The
// chip shows where the next dispatch goes (`companion.activeTarget`,
// G-STATE), speaking in seats: `@lead · claude · session 3`. The picker lists
// SEATS first (with their state dots), then UNSEATED sessions, then
// *New session on <engine>* and *Persistent run* (D-09 rule 5). Picking a
// seat or a session selects its rail row too (target + panel scope, G-SEATS
// §9.1); a new-session / persistent row moves only the chip. ⌥↑ / ⌥↓ walk
// the same list (`cycleTargets`, the Companion's handler).
// Right-click offers "Copy address" / "Copy session id" and "Open session in
// a pane".

import { useQuery } from '@tanstack/react-query';
import { useNavigate } from '@tanstack/react-router';
import { Bot, ChevronDown, ChevronRight, User } from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { EmptyState, OfflineState } from '@/components/states';
import { usePaneStore } from '@/lib/panes/pane-store';
import { type CompanionTarget, useShellStore } from '@/lib/shell/shell-store';
import { useSeats } from '@/lib/queries/seats';
import { chiList, type DetectedAgent, detectAgents, type SeatStatus, type SeatView } from '@/lib/tauri-cmd';
import { viewLabel } from '@/shell/panes/pane-views';
import { useTerminalStore } from '@/terminal/session-store';
import { createTerminalSession } from '@/terminal/single-terminal';
import { useTerminalTitles } from '@/terminal/use-terminal-titles';
import { useCompanionStore } from './companion-store';
import { applyTarget, copyText, openSessionInPane, sameTarget } from './seat-actions';
import { atName, engineShort, seatChipRest, seatSessionRef, stateDotColor, UNREPORTED } from './seat-model';
import { type SeatRoster, terminalEngine, type UnseatedSession } from './seat-roster';
import { figuresOf, sessionName, useSessionFiguresStore } from './seat-sessions';
import { copyText as copyToClipboard } from '@/lib/clipboard';

type Group = 'Seats' | 'Unseated' | 'New session on…' | 'Persistent run' | 'Seat' | 'Session';

interface PickerItem {
	id: string;
	label: string;
	/** Trailing muted text (`claude · session 3`, `vacant`, `2k ctx`). */
	sub?: string;
	title?: string;
	group: Group;
	/** A state dot (seats and sessions). */
	dot?: SeatStatus;
	target?: CompanionTarget;
	run?: () => void;
	selected?: boolean;
}

/** The cursor value meaning "start on the current target". */
const CURSOR_CURRENT = -2;

/** The detected-engine query the picker (and the ⌥↑/⌥↓ cycle) share. */
export const DETECT_AGENTS_KEY = ['settings', 'agent', 'detect'] as const;

/** Engines a *New session on…* / *Persistent run* row may name: the default
 *  first, then every detected engine that isn't known to be signed out. */
export function offeredEngines(defaultEngineId: string | null, detected: readonly DetectedAgent[] | undefined): string[] {
	const signedOut = (id: string) => detected?.find((a) => a.id === id)?.authed === false;
	const out: string[] = [];
	if (defaultEngineId && !signedOut(defaultEngineId)) out.push(defaultEngineId);
	for (const a of detected ?? []) {
		if (a.authed === false || out.includes(a.id)) continue;
		out.push(a.id);
	}
	return out;
}

/** ⌥↑ / ⌥↓: seats, then unseated sessions, then new-on each engine, then a
 *  persistent run on the default engine (D-09 `pickerTargets`). */
export function cycleTargets(
	seats: readonly SeatView[],
	unseated: readonly Pick<UnseatedSession, 'id'>[],
	engines: readonly string[]
): CompanionTarget[] {
	const out: CompanionTarget[] = seats.map((s) => ({ kind: 'seat', seat_id: s.id }));
	for (const u of unseated) out.push({ kind: 'session', session_id: u.id });
	for (const e of engines) out.push({ kind: 'new', engine_id: e });
	if (engines[0]) out.push({ kind: 'persistent', engine_id: engines[0] });
	return out;
}

/** The chip's two halves: the address (`@lead`, emphasised) and the rest. */
export function useTargetParts(target: CompanionTarget): { at: string | null; rest: string } {
	const resolveTerminal = useTerminalTitles();
	const defaultEngineId = useShellStore((s) => s.defaultEngineId);
	const projectId = useShellStore((s) => s.activeProject.id);
	const tab = useTerminalStore((s) =>
		target.kind === 'session' ? s.tabs.find((t) => t.id === target.session_id) : undefined
	);
	// G-SEATS §9.4: a seat target labels from the roster cache (`@name`, §1.3).
	const seats = useSeats(projectId ?? null, { enabled: target.kind === 'seat' });
	switch (target.kind) {
		case 'session': {
			const n = sessionName(target.session_id);
			if (!tab) return { at: null, rest: `run · ${n}` };
			const engine = terminalEngine(tab);
			const who = engine
				? engineShort(engine)
				: viewLabel({ kind: 'terminal', sessionId: target.session_id }, resolveTerminal);
			return { at: null, rest: `${who} · ${n}` };
		}
		case 'new':
			return { at: null, rest: `New session · ${target.engine_id ?? defaultEngineId ?? 'no engine'}` };
		case 'persistent':
			return { at: null, rest: `Persistent run · ${target.engine_id ?? defaultEngineId ?? 'no engine'}` };
		case 'seat': {
			const seat = seats.data?.find((s) => s.id === target.seat_id);
			if (!seat) return { at: null, rest: seats.data ? 'Seat removed' : 'Seat…' };
			const ref = seatSessionRef(seat);
			return { at: atName(seat.name), rest: seatChipRest(seat, ref ? sessionName(ref) : null) };
		}
	}
}

/** Human label for a target, shared by the chip and the header caption. */
export function useTargetLabel(target: CompanionTarget): string {
	const { at, rest } = useTargetParts(target);
	return `${at ?? ''}${at ? rest : rest.replace(/^ · /, '')}`;
}

function Dot({ status }: { status: SeatStatus }) {
	return (
		<span
			aria-hidden="true"
			className="size-2 shrink-0 rounded-full"
			style={{
				background: stateDotColor(status),
				boxShadow: status === 'vacant' ? 'inset 0 0 0 1.5px var(--fg-muted)' : undefined,
			}}
		/>
	);
}

export function TargetPicker({ roster }: { roster: SeatRoster }) {
	const target = useShellStore((s) => s.companion.activeTarget);
	const defaultEngineId = useShellStore((s) => s.defaultEngineId);
	const terminals = useTerminalStore((s) => s.tabs);
	const pickerPending = useCompanionStore((s) => s.pickerPending);
	const consumePicker = useCompanionStore((s) => s.consumePicker);
	const resolveTerminal = useTerminalTitles();
	const snaps = useSessionFiguresStore((s) => s.snaps);
	const { at, rest } = useTargetParts(target);
	const label = `${at ?? ''}${at ? rest : rest.replace(/^ · /, '')}`;
	const navigate = useNavigate();

	const [open, setOpen] = useState<null | 'targets' | 'session'>(null);
	const [cursor, setCursor] = useState(0);
	const chipRef = useRef<HTMLButtonElement | null>(null);
	const menuRef = useRef<HTMLDivElement | null>(null);

	// The scoped panels' empty action asks for the picker (`openTargetPicker`).
	useEffect(() => {
		if (!pickerPending) return;
		consumePicker();
		setOpen('targets');
		setCursor(0);
	}, [pickerPending, consumePicker]);

	const engines = useQuery({
		queryKey: DETECT_AGENTS_KEY,
		queryFn: detectAgents,
		enabled: open === 'targets',
		refetchOnWindowFocus: false,
	});
	const runs = useQuery({
		queryKey: ['companion', 'chi-runs'],
		queryFn: () => chiList(null, 10),
		enabled: open === 'targets',
		refetchOnWindowFocus: false,
	});

	const engineIds = useMemo(() => offeredEngines(defaultEngineId, engines.data), [defaultEngineId, engines.data]);

	// The first known-unauthenticated engine among the ones that would
	// otherwise be offered — drives the "signed out" empty state below.
	const unauthedEngine = useMemo(() => {
		const ids = new Set<string>();
		if (defaultEngineId) ids.add(defaultEngineId);
		for (const a of engines.data ?? []) ids.add(a.id);
		for (const id of ids) {
			const agent = engines.data?.find((a) => a.id === id);
			if (agent?.authed === false) return agent;
		}
		return null;
	}, [defaultEngineId, engines.data]);

	const items = useMemo<PickerItem[]>(() => {
		if (open === 'session') {
			const out: PickerItem[] = [];
			if (target.kind === 'seat') {
				const seat = roster.seats.find((s) => s.id === target.seat_id);
				if (seat) {
					out.push({
						id: 'copy-address',
						group: 'Seat',
						label: 'Copy address',
						sub: atName(seat.name),
						run: () => copyText(atName(seat.name), `Copied ${atName(seat.name)}`),
					});
					const tid = seat.session?.kind === 'terminal' && seat.status !== 'vacant' ? seat.session.terminal_id : null;
					if (tid) {
						out.push({ id: 'open', group: 'Seat', label: 'Open session in a pane', run: () => openSessionInPane(tid) });
					}
				}
				return out;
			}
			if (target.kind !== 'session') return [];
			const sessionId = target.session_id;
			out.push({
				id: 'copy',
				group: 'Session',
				label: 'Copy session id',
				run: () => void copyToClipboard(sessionId),
			});
			if (terminals.some((t) => t.id === sessionId)) {
				out.push({ id: 'open', group: 'Session', label: 'Open session in a pane', run: () => openSessionInPane(sessionId) });
			}
			return out;
		}
		const out: PickerItem[] = [];
		for (const seat of roster.seats) {
			const tgt: CompanionTarget = { kind: 'seat', seat_id: seat.id };
			const ref = seatSessionRef(seat);
			const sub =
				seat.status === 'vacant'
					? 'vacant'
					: seat.status === 'run'
						? `run · ${ref ? sessionName(ref) : ''}`
						: `${engineShort(seat.engine_id)} · ${ref ? sessionName(ref) : 'session'}`;
			out.push({
				id: `seat:${seat.id}`,
				group: 'Seats',
				label: atName(seat.name),
				sub,
				dot: seat.status,
				target: tgt,
				selected: sameTarget(tgt, target),
			});
		}
		for (const u of roster.unseated) {
			const tgt: CompanionTarget = { kind: 'session', session_id: u.id };
			const live = u.status === 'running' || u.status === 'spawning';
			const ctx = figuresOf(snaps[u.id]).ctx;
			out.push({
				id: `s:${u.id}`,
				group: 'Unseated',
				label: `${u.engineId ? engineShort(u.engineId) : viewLabel({ kind: 'terminal', sessionId: u.id }, resolveTerminal)} · ${sessionName(u.id)}`,
				sub: `${ctx ?? '—'} ctx`,
				title: ctx ? undefined : `context ${UNREPORTED}`,
				dot: live ? 'live' : 'idle',
				target: tgt,
				selected: sameTarget(tgt, target),
			});
		}
		// Plain running terminals that are neither seated nor agent sessions stay
		// reachable (they were the picker's "Send to" rows before seats).
		const listed = new Set(roster.unseated.map((u) => u.id));
		const seatedIds = new Set(
			roster.seats.flatMap((s) => (s.session?.kind === 'terminal' ? [s.session.terminal_id] : []))
		);
		for (const t of terminals) {
			if (t.status !== 'running' && t.status !== 'spawning') continue;
			if (listed.has(t.id) || seatedIds.has(t.id)) continue;
			const tgt: CompanionTarget = { kind: 'session', session_id: t.id };
			out.push({
				id: `s:${t.id}`,
				group: 'Unseated',
				label: `${viewLabel({ kind: 'terminal', sessionId: t.id }, resolveTerminal)} · ${sessionName(t.id)}`,
				dot: 'live',
				target: tgt,
				selected: sameTarget(tgt, target),
			});
		}
		const seatedRuns = new Set(roster.seats.flatMap((s) => (s.session?.kind === 'run' ? [s.session.run_id] : [])));
		for (const r of runs.data ?? []) {
			if (!r.external_id || seatedRuns.has(r.run_id)) continue; // nothing to resume against
			const tgt: CompanionTarget = { kind: 'session', session_id: r.run_id };
			out.push({
				id: `r:${r.run_id}`,
				group: 'Unseated',
				label: `${engineShort(r.engine_id)} · ${sessionName(r.run_id)}`,
				sub: r.status,
				dot: r.status === 'running' || r.status === 'queued' ? 'run' : 'idle',
				target: tgt,
				selected: sameTarget(tgt, target),
			});
		}
		for (const id of engineIds) {
			const tgt: CompanionTarget = { kind: 'new', engine_id: id };
			out.push({
				id: `n:${id}`,
				group: 'New session on…',
				label: id,
				sub: id === defaultEngineId ? 'default' : undefined,
				target: tgt,
				selected: sameTarget(tgt, target),
			});
		}
		for (const id of engineIds) {
			const tgt: CompanionTarget = { kind: 'persistent', engine_id: id };
			out.push({
				id: `p:${id}`,
				group: 'Persistent run',
				label: id,
				sub: id === defaultEngineId ? '⌥↵' : undefined,
				target: tgt,
				selected: sameTarget(tgt, target),
			});
		}
		return out;
	}, [open, target, terminals, runs.data, engineIds, defaultEngineId, roster.seats, roster.unseated, resolveTerminal, snaps]);

	useEffect(() => {
		if (!open) return;
		const onDown = (e: MouseEvent) => {
			const t = e.target as Node | null;
			if (t && !chipRef.current?.contains(t) && !menuRef.current?.contains(t)) setOpen(null);
		};
		window.addEventListener('mousedown', onDown);
		return () => window.removeEventListener('mousedown', onDown);
	}, [open]);

	// Roving focus inside the open menu. `items` is a deliberate extra
	// dependency: the engine / run queries resolve after the menu opens, and
	// the cursor must re-land once their rows exist.
	// biome-ignore lint/correctness/useExhaustiveDependencies: see above
	useEffect(() => {
		if (!open) return;
		const els = menuRef.current?.querySelectorAll<HTMLElement>('[role^="menuitem"]');
		if (!els?.length) return;
		// `-2`: a click opened it — start on the current target (D-09).
		if (cursor === CURSOR_CURRENT) {
			const i = items.findIndex((it) => it.selected);
			setCursor(i >= 0 ? i : 0);
			return;
		}
		// `-1` means "the last item" — how the chip's ArrowUp opens the menu.
		if (cursor < 0) {
			setCursor(els.length - 1);
			return;
		}
		els[cursor]?.focus();
	}, [open, cursor, items]);

	function close() {
		setOpen(null);
		chipRef.current?.focus();
	}

	function pick(item: PickerItem) {
		if (item.target) applyTarget(item.target);
		item.run?.();
		close();
	}

	function onMenuKey(e: React.KeyboardEvent) {
		if (e.key === 'Home') {
			e.preventDefault();
			setCursor(0);
		} else if (e.key === 'End') {
			e.preventDefault();
			setCursor(items.length ? items.length - 1 : 0);
		} else if (e.key === 'ArrowDown') {
			e.preventDefault();
			setCursor((c) => (items.length ? (c + 1) % items.length : 0));
		} else if (e.key === 'ArrowUp') {
			e.preventDefault();
			setCursor((c) => (items.length ? (c - 1 + items.length) % items.length : 0));
		} else if (e.key === 'Escape') {
			e.preventDefault();
			e.stopPropagation();
			close();
		}
	}

	// The menu's children must be `menuitem*` or `group` for the roles to be
	// owned (WAI-ARIA `menu` required-children). Runs of the same `group`
	// collapse into one `role="group"`, and the caption inside it is
	// aria-hidden because the group's own accessible name already carries it.
	const groups = useMemo(() => {
		const out: { group: string; items: PickerItem[] }[] = [];
		for (const item of items) {
			const last = out.at(-1);
			if (last && last.group === item.group) last.items.push(item);
			else out.push({ group: item.group, items: [item] });
		}
		return out;
	}, [items]);

	return (
		<div className="relative mb-2 max-w-full">
			<button
				ref={chipRef}
				type="button"
				aria-haspopup="menu"
				aria-expanded={open === 'targets'}
				aria-label={`Dispatch target: ${label}. Change it (⌥↑ / ⌥↓ cycle).`}
				onClick={() => {
					setOpen((o) => (o === 'targets' ? null : 'targets'));
					// D-09: the cursor starts on the current target (the roving
					// effect lands it once the rows exist).
					setCursor(CURSOR_CURRENT);
				}}
				onKeyDown={(e) => {
					// APG menu button: Down opens on the first item, Up on the last.
					if (e.altKey) return; // ⌥↑ / ⌥↓ cycle the target instead
					if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
					e.preventDefault();
					setOpen('targets');
					setCursor(e.key === 'ArrowDown' ? 0 : -1);
				}}
				onContextMenu={(e) => {
					if (target.kind !== 'session' && target.kind !== 'seat') return;
					e.preventDefault();
					setOpen('session');
					setCursor(0);
				}}
				className="inline-flex border-[var(--border)] bg-[var(--bg-raised)] text-[var(--fg-muted)] hover:border-[var(--border-strong)] hover:text-[var(--fg)] aria-expanded:border-[var(--primary)] h-6 max-w-full items-center gap-1 rounded-full border px-2 text-[11px] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
			>
				<ChevronRight className="h-3 w-3 shrink-0" aria-hidden="true" />
				<span className="truncate font-mono">
					{at && (
						<span className="font-medium" style={{ color: 'var(--fg)' }}>
							{at}
						</span>
					)}
					<span style={{ color: at ? 'var(--fg-muted)' : 'var(--fg)' }}>{at ? rest : rest.replace(/^ · /, '')}</span>
				</span>
				<ChevronDown className="h-3 w-3 shrink-0" aria-hidden="true" />
			</button>
			{open && (
				<div
					ref={menuRef}
					role="menu"
					aria-label={open === 'targets' ? 'Dispatch targets' : target.kind === 'seat' ? 'Seat actions' : 'Session actions'}
					data-state={open === 'targets' ? 'seats-dispatch' : undefined}
					onKeyDown={onMenuKey}
					className="absolute left-0 top-full z-50 mt-1 max-h-80 w-72 overflow-y-auto rounded-md border py-1 shadow-lg"
					style={{ background: 'var(--bg-raised)', borderColor: 'var(--border)' }}
				>
					{open === 'targets' &&
						items.length === 0 &&
						(unauthedEngine ? (
							<OfflineState
								data-state="companion-signed-out"
								icon={User}
								heading={`${unauthedEngine.display} is not signed in`}
								body={
									unauthedEngine.auth_hint ??
									`The binary is on your PATH at ${unauthedEngine.executable_path}. It just has no credentials.`
								}
								action={{
									label: `Run ${unauthedEngine.id} login`,
									onClick: () => {
										const sessionId = createTerminalSession({
											cmd: [unauthedEngine.executable_path, 'login'],
											title: `${unauthedEngine.id} login`,
										});
										const panes = usePaneStore.getState();
										panes.placeView(panes.focusedId, { kind: 'terminal', sessionId }, 'append');
										close();
									},
								}}
								className="min-h-0"
							/>
						) : (
							<EmptyState
								data-state="companion-no-engine"
								icon={Bot}
								heading="No engine installed"
								body="The shell works without one — panes, artifacts and packages are all still yours. Chi needs an engine to have a seat."
								action={{
									label: 'Install claude-code',
									onClick: () => {
										close();
										void navigate({ to: '/packages', search: { filter: 'store' } });
									},
								}}
								className="min-h-0"
							/>
						))}
					{groups.map(({ group, items: groupItems }) => (
						// A <fieldset> — what the rule suggests — is not a valid child of
						// role="menu". ARIA's grouping element inside a menu is a div with
						// role="group".
						// biome-ignore lint/a11y/useSemanticElements: see above
						<div key={group} role="group" aria-label={group}>
							{open === 'targets' && (
								// The group's accessible name already says this; repeating it as
								// a text node would have AT read every caption twice.
								<div
									aria-hidden="true"
									className="px-3 pb-0.5 pt-1.5 text-[11px] font-semibold uppercase tracking-wider"
									style={{ color: 'var(--fg-muted)' }}
								>
									{group}
								</div>
							)}
							{groupItems.map((item) => {
								const i = items.indexOf(item);
								return (
									<button
										key={item.id}
										type="button"
										{...(item.target
											? { role: 'menuitemradio', 'aria-checked': Boolean(item.selected) }
											: { role: 'menuitem' })}
										tabIndex={i === cursor ? 0 : -1}
										title={item.title}
										onClick={() => pick(item)}
										className="flex text-[var(--fg)] hover:bg-[var(--bg-sunken)] min-h-6 w-full items-center gap-2 px-3 py-1 text-left text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
									>
										{item.dot && <Dot status={item.dot} />}
										<span className={item.group === 'Seats' ? 'truncate font-mono' : 'truncate'}>{item.label}</span>
										{(item.sub || item.selected) && (
											<span className="ml-auto flex shrink-0 items-center gap-2 pl-2 text-[11px]" style={{ color: 'var(--fg-muted)' }}>
												{item.sub && <span className="truncate">{item.sub}</span>}
												{item.selected && <span>current</span>}
											</span>
										)}
									</button>
								);
							})}
						</div>
					))}
				</div>
			)}
		</div>
	);
}
