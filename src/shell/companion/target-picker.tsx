// Target chip + picker — spec §3.10 #62. The chip shows where the next
// dispatch goes (`companion.activeTarget`, G-STATE); clicking opens a menu of
// running sessions · "New session on <engine>" · "Persistent run on <engine>".
// Right-click offers "Copy session id" / "Open session in a pane" for a
// session target.

import { useQuery } from '@tanstack/react-query';
import { useNavigate } from '@tanstack/react-router';
import { Bot, ChevronDown, ChevronRight, User } from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { EmptyState, OfflineState } from '@/components/states';
import { usePaneStore } from '@/lib/panes/pane-store';
import { type CompanionTarget, useShellStore } from '@/lib/shell/shell-store';
import { chiList, type DetectedAgent, detectAgents } from '@/lib/tauri-cmd';
import { viewLabel } from '@/shell/panes/pane-views';
import { useTerminalStore } from '@/terminal/session-store';
import { createTerminalSession } from '@/terminal/single-terminal';
import { useTerminalTitles } from '@/terminal/use-terminal-titles';
import { useCompanionStore } from './companion-store';

interface PickerItem {
	id: string;
	label: string;
	group: 'Send to' | 'New session on…' | 'Persistent run on…' | 'Session';
	target?: CompanionTarget;
	run?: () => void;
	selected?: boolean;
}

function sameTarget(a: CompanionTarget, b: CompanionTarget): boolean {
	if (a.kind !== b.kind) return false;
	if (a.kind === 'session' && b.kind === 'session') return a.session_id === b.session_id;
	if (a.kind !== 'session' && b.kind !== 'session') return a.engine_id === b.engine_id;
	return false;
}

/** Human label for a target, shared by the chip and the header caption. */
export function useTargetLabel(target: CompanionTarget): string {
	const resolveTerminal = useTerminalTitles();
	const defaultEngineId = useShellStore((s) => s.defaultEngineId);
	const hasTerminal = useTerminalStore((s) =>
		target.kind === 'session' ? s.tabs.some((t) => t.id === target.session_id) : false
	);
	switch (target.kind) {
		case 'session':
			return hasTerminal
				? viewLabel({ kind: 'terminal', sessionId: target.session_id }, resolveTerminal)
				: `run ${target.session_id.slice(0, 8)}`;
		case 'new':
			return `New session · ${target.engine_id ?? defaultEngineId ?? 'no engine'}`;
		case 'persistent':
			return `Persistent run · ${target.engine_id ?? defaultEngineId ?? 'no engine'}`;
	}
}

export function TargetPicker() {
	const target = useShellStore((s) => s.companion.activeTarget);
	const setTarget = useShellStore((s) => s.setCompanionTarget);
	const defaultEngineId = useShellStore((s) => s.defaultEngineId);
	const terminals = useTerminalStore((s) => s.tabs);
	const setScope = useCompanionStore((s) => s.setPanelScope);
	const pickerPending = useCompanionStore((s) => s.pickerPending);
	const consumePicker = useCompanionStore((s) => s.consumePicker);
	const resolveTerminal = useTerminalTitles();
	const label = useTargetLabel(target);
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
		queryKey: ['settings', 'agent', 'detect'],
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

	// Detected agents keyed by id, so the "New session on…" / "Persistent run
	// on…" rows can skip an engine that's installed but not signed in — D-07's
	// "signed out" cell replaces the broken row rather than sitting beside it.
	const detectedById = useMemo(() => {
		const m = new Map<string, DetectedAgent>();
		for (const a of engines.data ?? []) m.set(a.id, a);
		return m;
	}, [engines.data]);

	const engineIds = useMemo(() => {
		const set = new Set<string>();
		if (defaultEngineId) set.add(defaultEngineId);
		for (const a of engines.data ?? []) set.add(a.id);
		return set;
	}, [defaultEngineId, engines.data]);

	// The first known-unauthenticated engine among the ones that would
	// otherwise be offered — drives the "signed out" empty state below.
	const unauthedEngine = useMemo(() => {
		for (const id of engineIds) {
			const agent = detectedById.get(id);
			if (agent?.authed === false) return agent;
		}
		return null;
	}, [engineIds, detectedById]);

	const items = useMemo<PickerItem[]>(() => {
		if (open === 'session') {
			if (target.kind !== 'session') return [];
			const sessionId = target.session_id;
			const out: PickerItem[] = [
				{
					id: 'copy',
					group: 'Session',
					label: 'Copy session id',
					run: () => void navigator.clipboard?.writeText(sessionId).catch(() => {}),
				},
			];
			if (terminals.some((t) => t.id === sessionId)) {
				out.push({
					id: 'open',
					group: 'Session',
					label: 'Open session in a pane',
					run: () => {
						const panes = usePaneStore.getState();
						panes.placeView(panes.focusedId, { kind: 'terminal', sessionId }, 'append');
					},
				});
			}
			return out;
		}
		const out: PickerItem[] = [];
		for (const t of terminals) {
			if (t.status !== 'running' && t.status !== 'spawning') continue;
			const tgt: CompanionTarget = { kind: 'session', session_id: t.id };
			out.push({
				id: `s:${t.id}`,
				group: 'Send to',
				label: viewLabel({ kind: 'terminal', sessionId: t.id }, resolveTerminal),
				target: tgt,
				selected: sameTarget(tgt, target),
			});
		}
		for (const r of runs.data ?? []) {
			if (!r.external_id) continue; // nothing to resume against
			const tgt: CompanionTarget = { kind: 'session', session_id: r.run_id };
			out.push({
				id: `r:${r.run_id}`,
				group: 'Send to',
				label: `${r.engine_id} · run ${r.run_id.slice(0, 8)} · ${r.status}`,
				target: tgt,
				selected: sameTarget(tgt, target),
			});
		}
		for (const id of engineIds) {
			if (detectedById.get(id)?.authed === false) continue; // → the "signed out" empty state
			const tgt: CompanionTarget = { kind: 'new', engine_id: id };
			out.push({
				id: `n:${id}`,
				group: 'New session on…',
				label: id,
				target: tgt,
				selected: sameTarget(tgt, target),
			});
		}
		for (const id of engineIds) {
			if (detectedById.get(id)?.authed === false) continue;
			const tgt: CompanionTarget = { kind: 'persistent', engine_id: id };
			out.push({
				id: `p:${id}`,
				group: 'Persistent run on…',
				label: id,
				target: tgt,
				selected: sameTarget(tgt, target),
			});
		}
		return out;
	}, [open, target, terminals, runs.data, engineIds, detectedById, resolveTerminal]);

	useEffect(() => {
		if (!open) return;
		const onDown = (e: MouseEvent) => {
			const t = e.target as Node | null;
			if (t && !chipRef.current?.contains(t) && !menuRef.current?.contains(t)) setOpen(null);
		};
		window.addEventListener('mousedown', onDown);
		return () => window.removeEventListener('mousedown', onDown);
	}, [open]);

	// Roving focus inside the open menu. `items.length` is a deliberate extra
	// dependency: the engine / run queries resolve after the menu opens, and
	// the cursor must re-land once their rows exist.
	// biome-ignore lint/correctness/useExhaustiveDependencies: see above
	useEffect(() => {
		if (!open) return;
		const els = menuRef.current?.querySelectorAll<HTMLElement>('[role^="menuitem"]');
		if (!els?.length) return;
		// `-1` means "the last item" — how the chip's ArrowUp opens the menu.
		if (cursor < 0) {
			setCursor(els.length - 1);
			return;
		}
		els[cursor]?.focus();
	}, [open, cursor, items.length]);

	function close() {
		setOpen(null);
		chipRef.current?.focus();
	}

	function pick(item: PickerItem) {
		if (item.target) {
			setTarget(item.target);
			// Picking a session scopes the panels to it too (spec §3.11: the
			// selected session sets both).
			if (item.target.kind === 'session') setScope(item.target.session_id);
		}
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
	// owned (WAI-ARIA `menu` required-children). Items were previously wrapped
	// in a bare <div> each, with the group caption as a sibling div — so the
	// menu owned neither. Runs of the same `group` collapse into one
	// `role="group"`, and the caption inside it is aria-hidden because the
	// group's own accessible name already carries it.
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
				aria-label={`Dispatch target: ${label}`}
				onClick={() => {
					setOpen((o) => (o === 'targets' ? null : 'targets'));
					setCursor(0);
				}}
				onKeyDown={(e) => {
					// APG menu button: Down opens on the first item, Up on the last.
					if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
					e.preventDefault();
					setOpen('targets');
					setCursor(e.key === 'ArrowDown' ? 0 : -1);
				}}
				onContextMenu={(e) => {
					if (target.kind !== 'session') return;
					e.preventDefault();
					setOpen('session');
					setCursor(0);
				}}
				className="inline-flex border-[var(--border)] bg-[var(--bg-raised)] text-[var(--fg-muted)] hover:border-[var(--border-strong)] hover:text-[var(--fg)] aria-expanded:border-[var(--primary)] h-6 max-w-full items-center gap-1 rounded-full border px-2 text-[11px] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
			>
				<ChevronRight className="h-3 w-3 shrink-0" aria-hidden="true" />
				<span className="truncate font-mono" style={{ color: 'var(--fg)' }}>
					{label}
				</span>
				<ChevronDown className="h-3 w-3 shrink-0" aria-hidden="true" />
			</button>
			{open && (
				<div
					ref={menuRef}
					role="menu"
					aria-label={open === 'targets' ? 'Dispatch targets' : 'Session actions'}
					onKeyDown={onMenuKey}
					className="absolute left-0 top-full z-50 mt-1 max-h-80 w-64 overflow-y-auto rounded-md border py-1 shadow-lg"
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
									className="px-3 pb-0.5 pt-1.5 text-[10px] font-semibold uppercase tracking-wider"
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
										onClick={() => pick(item)}
										className="flex text-[var(--fg)] hover:bg-[var(--bg-sunken)] min-h-6 w-full items-center gap-2 px-3 py-1 text-left text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
									>
										<span className="truncate">{item.label}</span>
										{item.selected && (
											<span className="ml-auto text-[10px]" style={{ color: 'var(--fg-muted)' }}>
												current
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
