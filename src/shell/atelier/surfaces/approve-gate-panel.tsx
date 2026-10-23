/* approve-gate-panel — the run-then-pause draft-review surface.
 *
 * Renders the locked D-04 approve-gate (plans/atelier/designs/atelier-approve-gate.html) as a
 * shell surface: a resizable master-detail split (draft queue + editor + sticky action footer)
 * over the app-kit component classes + this surface's pattern-glue CSS.
 *
 * SEAM (the renderer is the sibling plans/atelier/ plan — 07-fe-button-renderer.md §3.5): the
 * run-then-pause behavior is injected via the typed props below. `drafts` is the pa-action-paused
 * payload; onApprove fires after the host's 10s undo window (host.paActionsCommit); onReject is
 * pa-action-reject. Until that plumbing lands, the fixture harness/tests stub these props. The
 * 10s countdown is rendered locally here (the shell toast component is not built yet). */

// biome-ignore-all lint/a11y/useSemanticElements: custom ARIA widget porting the locked D-04 a11y spec — rows are div[role=button] (they contain a nested checkbox <button>, so a real <button> would be invalid HTML); the resize handle is role=separator; the queue role=list; the footer role=toolbar; the undo toast role=status. All intentional.

import {
	type CSSProperties,
	type KeyboardEvent as ReactKeyboardEvent,
	type ReactNode,
	type PointerEvent as ReactPointerEvent,
	useCallback,
	useEffect,
	useLayoutEffect,
	useMemo,
	useRef,
	useState,
} from 'react';
import './approve-gate-panel.css';
import { CHANNEL_LABEL, type DraftChannel, type PausedDraft } from '@ikenga/contract';
import { focusMarkerProps } from '@/lib/keymap/context-keys';
import { useCommands } from '@/lib/keymap/dispatcher';
import {
	type DeliveryChipState,
	deliveryChipState,
	type PausedDraftView,
	type WorkerHealth,
} from '@/lib/queries/pa-actions';
import { copyText } from '@/lib/clipboard';
import type { PaDraftSeen } from '@/lib/tauri-cmd';
import { boundFields } from './share-binding';

// ── Types ─────────────────────────────────────────────────────────────────────────────────────
// PausedDraft / DraftChannel are the shared run-then-pause contract — the renderer injects them
// (07-fe-button-renderer.md §3.5; 10-approve-gate-seam.md WP-1). Re-exported so the fixtures +
// tests keep importing them from the panel.
export type { DraftChannel, PausedDraft };

export interface ApproveGatePanelProps {
	drafts: PausedDraftView[];
	/** Host fires the side effect after its 10s undo window (host.paActionsCommit).
	 *  In share mode (`shareNote` set) the second argument is the row the reviewer was
	 *  shown when they pressed Approve; the host sends it with the commit. */
	onApprove: (draftId: string, seen?: PaDraftSeen) => void;
	/** pa-action-reject — draft discarded. */
	onReject: (draftId: string) => void;
	/** Persisted inline edits (⌘S). Optional — the harness logs them. */
	onEdit?: (draftId: string, patch: { subject?: string; body?: string }) => void;
	/** Re-queue a failed draft for another worker attempt (WP-12 / G-09). */
	onRetry?: (draftId: string) => void;
	/**
	 * WP-11 — client-side worker-liveness snapshot (derived by the route from the
	 * same `paActionsList` query). When present and there is delivery-relevant
	 * activity, the panel renders the delivery-health strip above the split.
	 */
	health?: WorkerHealth;
	/**
	 * Share mode (box plan WP-5): set it to show this note above the queue and to turn on
	 * the share rules: approve carries the row that was shown, and a draft with unsaved
	 * local edits cannot be approved (the approval would not cover what is on screen).
	 * Leave it unset on the desktop and in your own workspace: nothing changes there.
	 */
	shareNote?: string;
	/** A failed approve / reject / edit, by draft id. Shown on the draft; the row comes
	 *  back into the queue if the panel had already removed it. */
	draftErrors?: Record<string, string>;
}

const LIST_W_DEFAULT = 420;
const LIST_W_MIN = 320;
const DETAIL_W_MIN = 480;

// ── Inline icons (faithful to D-04, zero dep) ─────────────────────────────────────────────────

type IconProps = { className?: string };
const svg = (path: ReactNode) => (p: IconProps) => (
	<svg
		className={p.className}
		viewBox="0 0 16 16"
		width="14"
		height="14"
		fill="none"
		stroke="currentColor"
		strokeWidth="1.5"
		strokeLinecap="round"
		strokeLinejoin="round"
		aria-hidden="true"
	>
		{path}
	</svg>
);
const IcoCheck = svg(<path d="M3.5 8.5l3 3 6-7" />);
const IcoX = svg(<path d="M4 4l8 8M12 4l-8 8" />);
const IcoPlus = svg(<path d="M8 3v10M3 8h10" />);
const IcoSort = svg(<path d="M5 3v10M5 13l-2-2M5 3l2 2M11 13V3M11 3l-2 2M11 13l2-2" />);
const IcoBack = svg(<path d="M10 3l-5 5 5 5" />);
const IcoClock = svg(
	<>
		<circle cx="8" cy="8" r="5.5" />
		<path d="M8 5v3l2 1.5" />
	</>
);
const IcoUp = svg(<path d="M4 10l4-4 4 4" />);
const IcoDown = svg(<path d="M4 6l4 4 4-4" />);
const IcoPencil = svg(<path d="M10.5 3.5l2 2L6 12l-3 .8.8-3z" />);
const IcoSeq = svg(
	<>
		<circle cx="4" cy="4" r="1.5" />
		<circle cx="4" cy="12" r="1.5" />
		<circle cx="12" cy="8" r="1.5" />
		<path d="M4 5.5v5M5.3 4.6L10.7 7M5.3 11.4L10.7 9" />
	</>
);
const IcoRefresh = svg(<path d="M13 8A5 5 0 1 1 8 3M13 3v5h-5" />);

// ── Component ─────────────────────────────────────────────────────────────────────────────────

export function ApproveGatePanel(props: ApproveGatePanelProps) {
	const { drafts, onApprove, onReject, onEdit, onRetry, health, shareNote, draftErrors } = props;
	const shareMode = shareNote !== undefined;
	// Notices the panel raises itself (an approval blocked by unsaved edits).
	const [localErrors, setLocalErrors] = useState<Record<string, string>>({});
	const errorFor = (id: string): string | undefined => localErrors[id] ?? draftErrors?.[id];

	const [resolved, setResolved] = useState<Set<string>>(() => new Set());
	// WP-11 — id of the row whose delivery-error popover is open (single-open).
	const [openErrId, setOpenErrId] = useState<string | null>(null);
	const visible = useMemo(() => drafts.filter((d) => !resolved.has(d.id)), [drafts, resolved]);

	const [selectedId, setSelectedId] = useState<string | null>(visible[0]?.id ?? null);
	const [checked, setChecked] = useState<Set<string>>(() => new Set());
	const [listWidth, setListWidth] = useState<number | null>(null);
	const [edits, setEdits] = useState<
		Record<
			string,
			{
				subject: string;
				body: string;
				everEdited: boolean;
				savedAt: string | null;
				/** Share mode: the row's text when ⌘S ran, to tell "the box has not caught up yet"
				 *  from "the box's draft changed after the save". */
				rowAtSave: { subject: string; body: string } | null;
			}
		>
	>({});
	const [undo, setUndo] = useState<{
		draftId: string;
		secondsLeft: number;
		seen?: PaDraftSeen;
	} | null>(null);

	const splitRef = useRef<HTMLDivElement>(null);
	const rowRefs = useRef<Map<string, HTMLDivElement>>(new Map());
	// The text a draft shows. On the desktop that is the local edit over the row. In a share it
	// is the row the approval will be bound to, except while the viewer is typing or their
	// save has not come back from the box yet: once the box's row is something other than
	// what they saved (the agent rewrote it), the screen follows the row, never the stale edit.
	const editFor = useCallback(
		(d: PausedDraft) => {
			const e = edits[d.id];
			if (!e) return undefined;
			if (!shareMode || e.savedAt === null) return e;
			const same = (a: { subject: string; body: string }) =>
				a.subject === d.subject && a.body === d.body;
			if (same(e) || (e.rowAtSave && same(e.rowAtSave))) return e;
			return undefined;
		},
		[edits, shareMode]
	);
	const selectedDraft = selectedId ? drafts.find((d) => d.id === selectedId) : undefined;
	const bodyRef = useAutoGrowTextarea(
		selectedDraft ? (editFor(selectedDraft)?.body ?? selectedDraft.body ?? '') : ''
	);

	// Keep selection valid as rows arrive/resolve. Auto-select the first draft
	// when nothing is selected (or the selection went stale) — `drafts` arrives
	// asynchronously from the renderer's query, so the initial useState seed runs
	// before there is anything to select. Clears the selection when the queue empties.
	useEffect(() => {
		if (visible.length === 0) {
			if (selectedId !== null) setSelectedId(null);
			return;
		}
		if (!selectedId || !visible.some((d) => d.id === selectedId)) {
			setSelectedId(visible[0].id);
		}
	}, [visible, selectedId]);

	const selected = visible.find((d) => d.id === selectedId) ?? null;

	// A failed action brings its draft back: the panel removes a row optimistically when
	// approve / reject fires, but nothing was done, so the draft must be on screen with
	// the reason.
	useEffect(() => {
		const ids = Object.keys(draftErrors ?? {});
		if (ids.length === 0) return;
		setResolved((prev) => {
			if (!ids.some((id) => prev.has(id))) return prev;
			const next = new Set(prev);
			for (const id of ids) next.delete(id);
			return next;
		});
		setSelectedId(ids[0]);
	}, [draftErrors]);

	// WP-11 — dismiss the delivery-error popover on any outside click or Escape.
	// The chip button + popover body stop propagation, so a bubbled click here is
	// always "outside". Only mounts a listener while a popover is open.
	useEffect(() => {
		if (!openErrId) return;
		const close = () => setOpenErrId(null);
		const onKey = (e: KeyboardEvent) => {
			if (e.key === 'Escape') setOpenErrId(null);
		};
		document.addEventListener('click', close);
		document.addEventListener('keydown', onKey);
		return () => {
			document.removeEventListener('click', close);
			document.removeEventListener('keydown', onKey);
		};
	}, [openErrId]);

	// display order = sections (Overdue → Today → This week), flattened.
	// J/K navigation and the "N of M" counter follow THIS order, not the prop order.
	const sections = useMemo(() => groupBySection(visible), [visible]);
	const flat = useMemo(() => sections.flatMap(([, rows]) => rows), [sections]);

	// derive the current (possibly edited) subject/body for a draft
	const draftSubject = (d: PausedDraft) => editFor(d)?.subject ?? d.subject;
	const draftBody = (d: PausedDraft) => editFor(d)?.body ?? d.body;

	// Share mode: a saved edit whose row the box then changed is dropped, and the viewer is told
	// that what is on screen is now the box's version.
	useEffect(() => {
		if (!shareMode) return;
		const stale = drafts.filter((d) => edits[d.id] && !editFor(d));
		if (stale.length === 0) return;
		setEdits((prev) => {
			const next = { ...prev };
			for (const d of stale) delete next[d.id];
			return next;
		});
		setLocalErrors((prev) => {
			const next = { ...prev };
			for (const d of stale)
				next[d.id] =
					'The draft on the box changed after your edit. This is the box’s version now; review it before approving.';
			return next;
		});
	}, [shareMode, drafts, edits, editFor]);
	const isEdited = (d: PausedDraft) => d.everEdited || !!edits[d.id]?.everEdited;

	// ── editing ──────────────────────────────────────────────────────────────────────────────
	const patchEdit = useCallback(
		(id: string, patch: { subject?: string; body?: string }, base: PausedDraft) => {
			setEdits((prev) => {
				const cur = prev[id] ?? {
					subject: base.subject,
					body: base.body,
					everEdited: base.everEdited,
					savedAt: null,
					rowAtSave: null,
				};
				// In a share, typing after a save makes the edit unsaved again.
				const savedAt = shareMode ? null : cur.savedAt;
				return { ...prev, [id]: { ...cur, ...patch, savedAt } };
			});
		},
		[shareMode]
	);

	// Intentionally NOT memoized: it reads the live `edits` through draftSubject/draftBody, so it
	// must close over the latest render. A [onEdit]-memoized version would persist stale values.
	const save = (d: PausedDraft) => {
		setEdits((prev) => {
			const cur = prev[d.id] ?? {
				subject: d.subject,
				body: d.body,
				everEdited: d.everEdited,
				savedAt: null,
				rowAtSave: null,
			};
			return {
				...prev,
				[d.id]: {
					...cur,
					everEdited: true,
					savedAt: nowLabel(),
					rowAtSave: { subject: d.subject, body: d.body },
				},
			};
		});
		onEdit?.(d.id, { subject: draftSubject(d), body: draftBody(d) });
	};

	// ── approve / reject / handoff ─────────────────────────────────────────────────────────────
	// Why this draft cannot be approved from a share right now, or null. The box approves the row
	// it stores, so the screen must be that row: no unsaved typing, no saved edit the box has not
	// yet returned, and a row the ledger can bind to.
	const shareBlock = useCallback(
		(d: PausedDraft): string | null => {
			const e = editFor(d);
			if (e && (e.subject !== d.subject || e.body !== d.body)) {
				return e.savedAt === null
					? 'You have unsaved edits. Save them (⌘S) first so the approval covers exactly what you see.'
					: 'Your saved edit has not come back from the box yet. Wait a moment, then approve what you see.';
			}
			const bound = boundFields((d as PausedDraftView).seen);
			return bound.ok ? null : bound.reason;
		},
		[editFor]
	);
	const startApprove = useCallback(
		(d: PausedDraft) => {
			const spent = approveBlockReason(d, !!onRetry);
			if (spent) {
				setLocalErrors((prev) => ({ ...prev, [d.id]: spent }));
				return;
			}
			const block = shareMode ? shareBlock(d) : null;
			if (block) {
				setLocalErrors((prev) => ({ ...prev, [d.id]: block }));
				return;
			}
			setLocalErrors((prev) => {
				if (!(d.id in prev)) return prev;
				const { [d.id]: _gone, ...rest } = prev;
				return rest;
			});
			setUndo({
				draftId: d.id,
				secondsLeft: Math.round((d.consequence.undoMs ?? 10000) / 1000),
				seen: (d as PausedDraftView).seen,
			});
		},
		[shareMode, shareBlock, onRetry]
	);

	const cancelUndo = useCallback(() => setUndo(null), []);

	// drive the undo countdown; commit (onApprove + hide row) when it hits 0
	useEffect(() => {
		if (!undo) return;
		if (undo.secondsLeft <= 0) {
			if (shareMode) onApprove(undo.draftId, undo.seen);
			else onApprove(undo.draftId);
			setResolved((prev) => new Set(prev).add(undo.draftId));
			setUndo(null);
			return;
		}
		const t = setTimeout(
			() => setUndo((u) => (u ? { ...u, secondsLeft: u.secondsLeft - 1 } : null)),
			1000
		);
		return () => clearTimeout(t);
	}, [undo, onApprove, shareMode]);

	const reject = useCallback(
		(id: string) => {
			onReject(id);
			setResolved((prev) => new Set(prev).add(id));
			setChecked((prev) => {
				const next = new Set(prev);
				next.delete(id);
				return next;
			});
		},
		[onReject]
	);

	// ── selection + bulk ────────────────────────────────────────────────────────────────────────
	const selectRow = useCallback((id: string) => setSelectedId(id), []);
	const toggleCheck = useCallback((id: string) => {
		setChecked((prev) => {
			const next = new Set(prev);
			if (next.has(id)) next.delete(id);
			else next.add(id);
			return next;
		});
	}, []);
	const clearChecked = useCallback(() => setChecked(new Set()), []);
	const approveAllChecked = useCallback(() => {
		// A share approval is one reviewed draft at a time, never rows the viewer has not opened.
		if (shareMode) return;
		const sent = new Set<string>();
		checked.forEach((id) => {
			const d = drafts.find((x) => x.id === id);
			if (d && approveBlockReason(d, !!onRetry)) return; // already approved / failed: nothing to approve
			sent.add(id);
			onApprove(id);
		});
		setResolved((prev) => new Set([...prev, ...sent]));
		setChecked(new Set());
	}, [checked, onApprove, shareMode, drafts, onRetry]);
	const rejectAllChecked = useCallback(() => {
		checked.forEach((id) => {
			onReject(id);
		});
		setResolved((prev) => new Set([...prev, ...checked]));
		setChecked(new Set());
	}, [checked, onReject]);

	// ── J / K row navigation (blocker 5) ─────────────────────────────────────────────────────────
	const move = useCallback(
		(delta: number) => {
			if (flat.length === 0) return;
			const idx = Math.max(
				0,
				flat.findIndex((d) => d.id === selectedId)
			);
			const next = flat[Math.min(flat.length - 1, Math.max(0, idx + delta))];
			if (next) {
				setSelectedId(next.id);
				rowRefs.current.get(next.id)?.focus();
			}
		},
		[flat, selectedId]
	);

	// ── divider drag-resize ───────────────────────────────────────────────────────────────────────
	const onDividerDown = useCallback((e: ReactPointerEvent) => {
		e.preventDefault();
		const split = splitRef.current;
		if (!split) return;
		split.classList.add('is-resizing');
		const onMove = (ev: PointerEvent) => {
			const rect = split.getBoundingClientRect();
			const raw = ev.clientX - rect.left;
			const max = rect.width - DETAIL_W_MIN - 4;
			setListWidth(Math.max(LIST_W_MIN, Math.min(raw, max)));
		};
		const onUp = () => {
			split.classList.remove('is-resizing');
			window.removeEventListener('pointermove', onMove);
			window.removeEventListener('pointerup', onUp);
		};
		window.addEventListener('pointermove', onMove);
		window.addEventListener('pointerup', onUp);
	}, []);
	const resetDivider = useCallback(() => setListWidth(null), []);
	const onDividerKey = useCallback((e: ReactKeyboardEvent) => {
		if (e.key === 'ArrowLeft')
			setListWidth((w) => Math.max(LIST_W_MIN, (w ?? LIST_W_DEFAULT) - 24));
		else if (e.key === 'ArrowRight') setListWidth((w) => (w ?? LIST_W_DEFAULT) + 24);
	}, []);

	// ── keyboard: ⌘S save · ⌘↵ approve · J/K navigate ──────────────────────
	// WP-56 (G-ACTIONS §10.2/§10.6): migrated from local `onKeyDown` handlers
	// to registry commands scoped by the new `approveGateFocus` key (B-21).
	// J/K exclude text inputs via `!inputFocus` in the registry `when`
	// (`defaults.ts`); ⌘S/⌘↵ fire even while editing a field, as before.
	useCommands({
		'approve-gate.next': () => move(1), // Next (j = down, vim convention)
		'approve-gate.prev': () => move(-1), // Previous (k = up, vim convention)
		'approve-gate.save': () => {
			if (selected) save(selected);
		},
		'approve-gate.approve': () => {
			if (selected) startApprove(selected);
		},
	});

	const selectedIndex = selected ? flat.findIndex((d) => d.id === selected.id) : -1;
	const consequence = selected ? consequenceStrings(selected) : null;

	const splitStyle =
		listWidth != null ? ({ '--list-w': `${listWidth}px` } as CSSProperties) : undefined;

	return (
		<section
			className="ob-surface"
			aria-label="Draft approvals"
			{...focusMarkerProps('approve-gate')}
		>
			{shareNote && (
				<div className="ob-share-note" role="note">
					{shareNote}
				</div>
			)}
			{health && shouldShowHealthStrip(health) && <DeliveryHealthStrip health={health} />}
			<div className="ob-split" ref={splitRef} style={splitStyle}>
				{/* ── Left: draft queue ─────────────────────────────────────────────── */}
				<div className="ob-list" role="list" aria-label="Drafts awaiting approval">
					<div className="ob-toolbar">
						<span className="ob-toolbar-title">Approvals</span>
						<span className="ob-toolbar-meta">
							{visible.length} · {visible.filter((d) => d.overdue).length} overdue
						</span>
						<span className="ob-toolbar-spacer" />
						<button type="button" className="btn-icon" aria-label="Sort drafts">
							<IcoSort />
						</button>
						<button type="button" className="btn-icon" aria-label="New draft">
							<IcoPlus />
						</button>
					</div>

					<div className="ob-filters" role="group" aria-label="Filter drafts">
						{['Mine', 'Today', 'Overdue', 'By channel', 'By sequence'].map((f, i) => (
							<button
								type="button"
								key={f}
								className={`ob-filter${i === 0 ? ' is-on' : ''}`}
								aria-pressed={i === 0}
							>
								{f}
							</button>
						))}
					</div>

					{checked.size > 0 && (
						<div className="ob-bulk">
							<span className="ob-bulk-count">{checked.size} selected</span>
							<span className="ob-bulk-spacer" />
							{!shareMode && (
								<button
									type="button"
									className="btn btn-sm ob-actions-primary"
									onClick={approveAllChecked}
								>
									Approve all
								</button>
							)}
							<button type="button" className="btn btn-sm" onClick={rejectAllChecked}>
								Reject all
							</button>
							<button type="button" className="btn btn-sm btn-ghost" onClick={clearChecked}>
								Clear
							</button>
						</div>
					)}

					{visible.length === 0 ? (
						<div className="atelier-state is-empty" role="status">
							No approvals pending
						</div>
					) : (
						sections.map(([section, rows]) => (
							<div key={section}>
								<div className="ob-section">
									{section}
									<span className="ob-section-count">· {rows.length}</span>
								</div>
								{rows.map((d) => (
									<div
										key={d.id}
										ref={(el) => {
											if (el) rowRefs.current.set(d.id, el);
											else rowRefs.current.delete(d.id);
										}}
										className={`ob-row status-${d.status}${selectedId === d.id ? ' is-selected' : ''}${checked.has(d.id) ? ' is-checked' : ''}`}
										role="button"
										tabIndex={0}
										aria-label={`${d.recipient} – ${draftSubject(d)}`}
										aria-current={selectedId === d.id ? 'true' : undefined}
										onClick={() => selectRow(d.id)}
										onKeyDown={(e) => {
											if (e.key === 'Enter' || e.key === ' ') {
												e.preventDefault();
												selectRow(d.id);
											}
										}}
									>
										<button
											type="button"
											className="ob-row-check"
											role="checkbox"
											aria-checked={checked.has(d.id)}
											aria-label={`Select ${d.recipient} draft`}
											onClick={(e) => {
												e.stopPropagation();
												toggleCheck(d.id);
											}}
										>
											<IcoCheck />
										</button>
										<div className="ob-row-tick" aria-hidden="true" />
										<div>
											<div className="ob-row-meta">
												<span className="ob-row-to">{d.recipient}</span>
												<span className={`ob-row-time${d.timeVariant ? ` ${d.timeVariant}` : ''}`}>
													{d.scheduledAt}
												</span>
											</div>
											<div className="ob-row-subject">{draftSubject(d)}</div>
											<div className="ob-row-preview">{d.bodyPreview}</div>
											<div className="ob-row-chips">
												<DeliveryChip
													draft={d}
													healthState={health?.state}
													open={openErrId === d.id}
													onToggleError={() => setOpenErrId((cur) => (cur === d.id ? null : d.id))}
													onRetry={onRetry}
												/>
												<span className={`ob-chip channel-${d.channel}`}>
													{CHANNEL_LABEL[d.channel]}
												</span>
												{isEdited(d) && <span className="ob-chip edited">EDITED</span>}
												{errorFor(d.id) && <span className="ob-chip error">NOT APPROVED</span>}
												<span className="ob-chip agent">{d.agent}</span>
												<span className={`ob-chip sender${d.cold ? ' cold' : ''}`}>
													{d.senderAddress}
												</span>
												{d.sequence && (
													<span className="ob-chip seq">
														Step {d.sequence.step}/{d.sequence.total} · {d.sequence.name}
													</span>
												)}
											</div>
										</div>
									</div>
								))}
							</div>
						))
					)}
				</div>

				{/* ── Divider ─────────────────────────────────────────────────────────── */}
				<div
					className="ob-divider"
					role="separator"
					aria-orientation="vertical"
					aria-label="Resize draft list"
					aria-valuenow={Math.round(listWidth ?? LIST_W_DEFAULT)}
					aria-valuemin={LIST_W_MIN}
					tabIndex={0}
					onPointerDown={onDividerDown}
					onDoubleClick={resetDivider}
					onKeyDown={onDividerKey}
				/>

				{/* ── Right: draft editor ─────────────────────────────────────────────── */}
				{/* Fix round 1: ⌘S / ⌘↵ pre-WP-56 only listened here (`.ob-detail`), not
				    across the whole section — `approveGateDetailFocus` restores that scope
				    (defaults.ts); J/K stay section-wide on `approveGateFocus`. */}
				<div className="ob-detail" {...focusMarkerProps('approve-gate-detail')}>
					{selected ? (
						<>
							<div className="ob-detail-toolbar">
								<button
									type="button"
									className="btn-icon"
									aria-label="Back to list"
									onClick={() => move(-1)}
								>
									<IcoBack />
								</button>
								<button type="button" className="btn-icon" aria-label="Schedule for later">
									<IcoClock />
								</button>
								<button
									type="button"
									className="btn-icon"
									aria-label="Reject draft"
									onClick={() => reject(selected.id)}
								>
									<IcoX />
								</button>
								<span className="ob-detail-toolbar-spacer" />
								<span className="ob-detail-toolbar-meta">
									{selectedIndex + 1} of {visible.length}
								</span>
								<button
									type="button"
									className="btn-icon"
									aria-label="Previous draft (K)"
									aria-keyshortcuts="k"
									onClick={() => move(-1)}
								>
									<IcoUp />
								</button>
								<button
									type="button"
									className="btn-icon"
									aria-label="Next draft (J)"
									aria-keyshortcuts="j"
									onClick={() => move(1)}
								>
									<IcoDown />
								</button>
							</div>

							<div className="ob-detail-body">
								{errorFor(selected.id) && (
									<div
										className="ob-failed-callout ob-approval-error"
										role="alert"
										aria-label="Approval error"
									>
										<span className="ob-failed-callout-label">Not approved</span>
										<span className="ob-approval-error-msg">{errorFor(selected.id)}</span>
									</div>
								)}
								{/* WP-11 — dead-worker note: retry re-queues safely but nothing claims it
								    until the worker is back. Shown only when a send-pipeline row is
								    selected while the worker reads dead. */}
								{health?.state === 'dead' &&
									selected.delivery != null &&
									isSendPipelineStatus(selected.delivery.dbStatus) && (
										<div className="dh-worker-note" role="alert">
											<strong>Send worker offline.</strong> Retry will re-queue this draft as{' '}
											<code>committed</code>, but nothing will pick it up until the worker is back.
											It is safe to retry now — the row waits in the queue.
										</div>
									)}
								{selected.status === 'failed' && (
									<div className="ob-failed-callout" role="alert" aria-label="Send failure">
										<span className="ob-failed-callout-label">
											Send failed
											{selected.attempts != null && selected.attempts > 0
												? ` · ${selected.attempts} attempt${selected.attempts === 1 ? '' : 's'}`
												: ''}
										</span>
										{selected.errorMessage && (
											<span className="ob-failed-callout-msg">{selected.errorMessage}</span>
										)}
										{onRetry && (
											<button
												type="button"
												className="btn btn-sm ob-failed-callout-retry"
												onClick={() => onRetry(selected.id)}
												aria-label="Retry sending this draft"
											>
												<IcoRefresh />
												<span className="btn-label">Retry</span>
											</button>
										)}
										{selected.delivery != null && (
											<span className="dh-attempt-facts">
												claimed <b>{formatClock(selected.delivery.claimedAt)}</b> · attempts{' '}
												<b>{selected.delivery.attempts}</b> · last attempt{' '}
												<b>{formatClock(selected.delivery.lastAttemptAt)}</b> · external_id{' '}
												<b>{selected.delivery.externalId ?? '—'}</b>
												{selected.delivery.externalId == null
													? ' (never accepted by provider — retry cannot double-send)'
													: ''}
											</span>
										)}
									</div>
								)}
								<div className="ob-detail-head">
									<div className="ob-detail-to-row">
										<div className="ob-avatar" aria-hidden="true">
											{initials(selected.recipient)}
										</div>
										<div>
											<div className="ob-detail-to-name">{selected.recipient}</div>
											<div className="ob-detail-to-addr">
												{selected.recipientEmail ?? '—'}
												{selected.tenantId ? ` · Tenant ${selected.tenantId}` : ''}
											</div>
										</div>
										<div className="ob-detail-to-from">
											<div>
												From <span>{selected.senderAddress}</span>
											</div>
											<div>via {selected.fromProvider}</div>
										</div>
									</div>

									<input
										className="ob-edit-subject"
										aria-label="Email subject"
										value={draftSubject(selected)}
										onChange={(e) => patchEdit(selected.id, { subject: e.target.value }, selected)}
									/>

									<div className="ob-detail-meta">
										<span
											className="ob-chip"
											style={{
												color: 'var(--live)',
												borderColor: 'var(--live-soft)',
												background: 'var(--live-soft)',
											}}
										>
											ux_mode · approve
										</span>
										<span className={`ob-chip channel-${selected.channel}`}>
											{CHANNEL_LABEL[selected.channel]}
										</span>
										<span className="ob-chip agent">
											mail · drafted by your Chi · {selected.model}
										</span>
										{selected.deal && (
											<span
												className="ob-chip"
												style={{
													color: 'var(--achievement)',
													borderColor: 'var(--achievement-soft)',
													background: 'var(--achievement-soft)',
												}}
											>
												Active deal · {selected.deal}
											</span>
										)}
										<span className="ob-chip">Thread {selected.threadCount}</span>
										<span
											className="ob-chip"
											style={{
												color: 'var(--tint-fg-active)',
												borderColor: 'color-mix(in srgb, var(--tint-fg-active) 30%, var(--border))',
											}}
										>
											{selected.scheduledLabel}
										</span>
									</div>
								</div>

								{shareMode && <ShareBoundFields seen={(selected as PausedDraftView).seen} />}

								{selected.sequence && (
									<div className="ob-seq-callout" role="region" aria-label="Sequence context">
										<IcoSeq />
										<strong>{selected.sequence.name}</strong> — Step {selected.sequence.step} of{' '}
										{selected.sequence.total} · {selected.sequence.recipients} recipients
										<div className="ob-seq-callout-actions">
											<button type="button" className="btn btn-sm">
												View sequence
											</button>
											<button type="button" className="btn btn-sm">
												Edit sequence
											</button>
										</div>
									</div>
								)}

								<div className="ob-quick-edit">
									<div className="ob-quick-edit-head">
										<IcoPencil />
										<span>Body · type to edit</span>
										<span className="ob-quick-edit-head-meta">
											{wordCount(draftBody(selected))} words ·{' '}
											{draftBody(selected).split('\n').length} lines
										</span>
									</div>
									<textarea
										ref={bodyRef}
										aria-label="Email body"
										value={draftBody(selected)}
										onChange={(e) => patchEdit(selected.id, { body: e.target.value }, selected)}
									/>
									<div className="ob-quick-edit-foot">
										<span>
											<kbd>⌘</kbd>
											<kbd>S</kbd> save
										</span>
										<span>
											<kbd>⌘</kbd>
											<kbd>↵</kbd> approve &amp; send
										</span>
										<span>
											<kbd>⌘</kbd>
											<kbd>K</kbd> dispatch
										</span>
										{edits[selected.id]?.savedAt && (
											<span className="saved">Saved {edits[selected.id]?.savedAt}</span>
										)}
									</div>
								</div>
							</div>

							{/* Sticky action footer (action-bar-quick component) */}
							<div className="ob-actions" role="toolbar" aria-label="Draft actions">
								<div className="ob-actions-cluster">
									<button type="button" className="btn btn-sm btn-ghost" onClick={() => move(-1)}>
										<IcoBack />
										<span className="btn-label">Back</span>
									</button>
								</div>
								<div className="ob-actions-cluster">
									<button type="button" className="btn btn-sm" aria-label="Reschedule draft">
										<IcoClock />
										<span className="btn-label">Reschedule</span>
									</button>
									<button type="button" className="btn btn-sm" onClick={() => reject(selected.id)}>
										<IcoX />
										<span className="btn-label">Reject</span>
									</button>
								</div>
								<span className="ob-actions-spacer" />
								<span className="ob-actions-meta">
									→ sends to {selected.consequence.target} ·{' '}
									{recipientCount(selected.consequence.recipients)}{' '}
									{recipientCount(selected.consequence.recipients) === 1
										? 'recipient'
										: 'recipients'}{' '}
									· {selected.consequence.channel} · {selected.consequence.time} · undo{' '}
									{Math.round(selected.consequence.undoMs / 1000)}s
								</span>
								<button
									type="button"
									className="btn ob-actions-primary"
									aria-keyshortcuts="Meta+Enter"
									aria-label={consequence?.aria}
									title={approveBlockReason(selected, !!onRetry) ?? consequence?.title}
									disabled={approveBlockReason(selected, !!onRetry) !== null}
									onClick={() => startApprove(selected)}
								>
									<IcoCheck />
									<span className="btn-label">Approve &amp; Send</span>
								</button>
							</div>
						</>
					) : (
						<div className="atelier-state is-empty" role="status">
							Select a draft to review
						</div>
					)}
				</div>
			</div>

			{undo && (
				<div className="ob-undo-toast" role="status">
					<span>
						Sending to{' '}
						{drafts.find((d) => d.id === undo.draftId)?.consequence.target ?? 'recipient'}…
					</span>
					<span className="ob-undo-toast-count">undo {undo.secondsLeft}s</span>
					<button type="button" className="btn btn-sm" onClick={cancelUndo}>
						Undo
					</button>
				</div>
			)}
		</section>
	);
}

// ── helpers ─────────────────────────────────────────────────────────────────────────────────

// Body editor sizing: a fixed-height textarea with an inner scrollbar reads as
// a cramped form field; sized to its content it reads as the email itself, and
// `.ob-detail` (the pane) owns the scrolling like a document. Re-fits on value
// and selection change and on pane resize (the split is user-draggable).
function useAutoGrowTextarea(value: string, minHeight = 240) {
	const ref = useRef<HTMLTextAreaElement>(null);
	const fit = useCallback(() => {
		const el = ref.current;
		if (!el) return;
		el.style.height = 'auto';
		el.style.height = `${Math.max(minHeight, el.scrollHeight)}px`;
	}, [minHeight]);
	useLayoutEffect(fit, [fit, value]);
	useEffect(() => {
		// jsdom (vitest) has no ResizeObserver; the value-keyed fit still runs.
		if (typeof ResizeObserver === 'undefined') return;
		const el = ref.current;
		if (!el) return;
		const ro = new ResizeObserver(fit);
		ro.observe(el.parentElement ?? el);
		return () => ro.disconnect();
	}, [fit]);
	return ref;
}

function groupBySection(drafts: PausedDraft[]): Array<[string, PausedDraft[]]> {
	const order = ['Overdue', 'Today', 'This week'];
	const map = new Map<string, PausedDraft[]>();
	for (const d of drafts) {
		const list = map.get(d.section) ?? [];
		list.push(d);
		map.set(d.section, list);
	}
	return [...map.entries()].sort((a, b) => {
		const ai = order.indexOf(a[0]);
		const bi = order.indexOf(b[0]);
		return (ai === -1 ? 99 : ai) - (bi === -1 ? 99 : bi);
	});
}

function initials(name: string): string {
	const parts = name
		.replace(/[^a-zA-ZÀ-ɏ ]/g, '')
		.trim()
		.split(/\s+/)
		.filter(Boolean);
	if (parts.length === 0) return '?';
	if (parts.length === 1) return parts[0].slice(0, 2).toUpperCase();
	return (parts[0][0] + parts[parts.length - 1][0]).toUpperCase();
}

function wordCount(s: string): number {
	const t = s.trim();
	return t ? t.split(/\s+/).length : 0;
}

// A draft's `recipients` is a count in the fixtures but an array in the worker's drafts; a
// non-number must never reach the DOM (an array as a React child throws and blanks the panel).
function recipientCount(n: unknown): number {
	if (typeof n === 'number' && Number.isFinite(n)) return n;
	if (Array.isArray(n)) return n.length;
	return 1;
}

// The full consequence, carried onto the primary's tooltip + accessible name so
// the gate's safety premise (who/where/when + undo) survives even when the meta
// row is space-constrained. The "Approve & Send —" prefix keeps the visible CTA
// in the accessible name.
function consequenceStrings(d: PausedDraft): { title: string; aria: string } {
	const n = recipientCount(d.consequence.recipients);
	const word = n === 1 ? 'recipient' : 'recipients';
	const undoS = Math.round((d.consequence.undoMs ?? 10000) / 1000);
	return {
		title: `→ sends to ${d.consequence.target} · ${n} ${word} · ${d.consequence.channel} · ${d.fromProvider} · ${d.consequence.time} · ${undoS}s undo`,
		aria: `Approve & Send — to ${d.consequence.target}, ${n} ${word}, via ${d.consequence.channel}, ${d.consequence.time}, ${undoS} second undo`,
	};
}

function nowLabel(): string {
	const d = new Date();
	return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

// ── WP-11 · delivery-health strip + per-row delivery chips ──────────────────────────────────────

const DIAG_CMD = 'systemctl --user status agent-scheduler';

const HEALTH_STATE_LABEL: Record<WorkerHealth['state'], string> = {
	alive: 'Alive',
	idle: 'Idle',
	degraded: 'Degraded',
	dead: 'No signal',
};

/** Suppress the strip when there's nothing delivery-relevant to report — an alive
 *  worker with an empty pipeline is unobservable, not newsworthy. */
function shouldShowHealthStrip(h: WorkerHealth): boolean {
	return (h.state !== 'alive' && h.state !== 'idle') || h.sending + h.queued + h.failed > 0;
}

/** Why a draft cannot be approved from where it stands, or null. A draft already approved,
 *  sending, sent or failed has used its approval: pressing Approve again can only be refused,
 *  and on a parked failure it reads as a button that "didn't work". */
export function approveBlockReason(d: PausedDraft, canRetry: boolean): string | null {
	const db = (d as PausedDraftView).delivery?.dbStatus ?? (d.status === 'failed' ? 'failed' : '');
	switch (db) {
		case 'failed':
			return canRetry
				? 'This draft failed to send. Use Retry to send it again.'
				: 'This draft failed to send and cannot be approved again. Ask for a new draft.';
		case 'committed':
			return 'Already approved: waiting for the send worker.';
		case 'sending':
			return 'Already approved: sending now.';
		case 'sent':
			return 'Already sent.';
		default:
			return null;
	}
}

function isSendPipelineStatus(dbStatus: string): boolean {
	return dbStatus === 'committed' || dbStatus === 'sending' || dbStatus === 'failed';
}

function formatDuration(ms: number | null): string {
	if (ms == null) return '—';
	const s = Math.max(0, Math.round(ms / 1000));
	if (s < 60) return `${s}s`;
	const m = Math.round(s / 60);
	if (m < 60) return `${m}m`;
	return `${Math.round(m / 60)}h`;
}

function formatMsAgo(ms: number | null): string {
	return ms == null ? 'never' : `${formatDuration(ms)} ago`;
}

function formatClock(iso: string | null): string {
	if (!iso) return '—';
	const t = Date.parse(iso);
	if (Number.isNaN(t)) return '—';
	const d = new Date(t);
	return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

function ageFrom(iso: string | null): string | null {
	if (!iso) return null;
	const t = Date.parse(iso);
	if (Number.isNaN(t)) return null;
	return formatDuration(Date.now() - t);
}

function healthFacts(h: WorkerHealth): ReactNode {
	if (h.state === 'degraded') {
		return (
			<>
				oldest queued draft waiting <b>{formatDuration(h.oldestQueuedMsAgo)}</b> (expected &lt; 2m)
				· last activity <b>{formatMsAgo(h.lastActivityMsAgo)}</b> · <b>{h.failuresThisHour}</b>{' '}
				failures this hour
			</>
		);
	}
	if (h.state === 'dead') {
		return (
			<>
				<b>{h.queued}</b> committed draft{h.queued === 1 ? '' : 's'} waiting · nothing claimed for{' '}
				<b>{formatDuration(h.lastActivityMsAgo ?? h.oldestQueuedMsAgo)}</b> · last activity{' '}
				<b>{formatClock(h.lastActivityAt)}</b>
			</>
		);
	}
	return (
		<>
			<b>{h.sending}</b> sending · <b>{h.queued}</b> queued · <b>{h.failed}</b> failed · last worker
			activity <b>{formatMsAgo(h.lastActivityMsAgo)}</b>
		</>
	);
}

function DeliveryHealthStrip({ health }: { health: WorkerHealth }) {
	const [copied, setCopied] = useState(false);
	const copyDiag = () => {
		void copyText(DIAG_CMD).then((ok) => {
			if (!ok) return;
			setCopied(true);
			setTimeout(() => setCopied(false), 1400);
		});
	};
	return (
		<div
			className={`dh-strip state-${health.state}`}
			role={health.state === 'dead' ? 'alert' : 'status'}
			aria-label="Send worker health"
		>
			<span className={`dh-lamp is-${health.state}`} aria-hidden="true" />
			<span className="dh-label">Send worker</span>
			<span className={`dh-state is-${health.state}`}>{HEALTH_STATE_LABEL[health.state]}</span>
			<span className="dh-facts">{healthFacts(health)}</span>
			<span className="dh-spacer" />
			{health.state === 'dead' && (
				<>
					<div className="dh-actions">
						<button type="button" className="btn btn-sm" onClick={copyDiag}>
							{copied ? 'Copied' : 'Copy diagnose cmd'}
						</button>
					</div>
					<div className="dh-dead-copy">
						<span>
							<strong>Approved drafts will not send until the worker is back.</strong> Approve &amp;
							Send still works — rows queue safely as <code>committed</code> and the worker claims
							them on recovery. The worker runs outside the shell in the agent-scheduler daemon
							(systemd user service): check <code>{DIAG_CMD}</code>.
						</span>
					</div>
				</>
			)}
		</div>
	);
}

/** Text label for the non-interactive delivery chips (queued/scheduled/stalled). */
function deliveryChipLabel(state: DeliveryChipState, d: { committedAt: string | null }): string {
	switch (state) {
		case 'scheduled':
			return 'scheduled · not yet due';
		case 'queued': {
			const age = ageFrom(d.committedAt);
			return age ? `queued · ${age}` : 'queued';
		}
		case 'stalled': {
			const age = ageFrom(d.committedAt);
			return age ? `no worker · ${age}` : 'no worker';
		}
		default:
			return state;
	}
}

/** Leading delivery chip on a queue row. Failed rows render an interactive
 *  `error_text` popover (Copy error + Retry reuse the existing wiring); every
 *  other state is a static status chip. */
function DeliveryChip({
	draft,
	healthState,
	open,
	onToggleError,
	onRetry,
}: {
	draft: PausedDraftView;
	healthState: WorkerHealth['state'] | undefined;
	open: boolean;
	onToggleError: () => void;
	onRetry?: (id: string) => void;
}) {
	const d = draft.delivery;
	const state = deliveryChipState(d, healthState);
	if (!state || !d) return null;

	if (state === 'sending') {
		const attempts = d.attempts || 0;
		return (
			<span className="dl-chip sending">
				<span className="dl-dot" aria-hidden="true" />
				sending{attempts > 0 ? ` · attempt ${attempts}` : ''}
			</span>
		);
	}

	if (state === 'sent') {
		const ds = d.deliveryStatus;
		const showDstat =
			ds === 'delivered' || ds === 'bounced' || ds === 'complained' || ds === 'errored';
		return (
			<>
				<span className="dl-chip sent">sent {formatClock(d.sentAt)}</span>
				{showDstat && (
					<span className={`dl-chip dstat-${ds === 'delivered' ? 'delivered' : 'bounced'}`}>
						{ds} · checked {formatClock(d.deliveryCheckedAt)}
					</span>
				)}
			</>
		);
	}

	if (state === 'failed') {
		const attempts = d.attempts || draft.attempts || 0;
		return (
			<span className="dl-pop-wrap">
				<button
					type="button"
					className="dl-chip failed"
					aria-expanded={open}
					aria-label={`Send failed, ${attempts} attempt${attempts === 1 ? '' : 's'} — show error`}
					onClick={(e) => {
						e.stopPropagation();
						onToggleError();
					}}
				>
					failed{attempts > 0 ? ` · ${attempts}×` : ''}
				</button>
				{open && (
					// biome-ignore lint/a11y/noStaticElementInteractions: popover swallows the row-select click/keydown so opening the error doesn't reselect the row
					<div
						className="dl-pop is-open"
						role="dialog"
						aria-label="Send error detail"
						onClick={(e) => e.stopPropagation()}
						onKeyDown={(e) => e.stopPropagation()}
					>
						<div className="dl-pop-title">
							<span>error_text{attempts > 0 ? ` · attempt ${attempts}` : ''}</span>
							<button
								type="button"
								className="btn-icon"
								aria-label="Close error detail"
								style={{ width: 20, height: 20 }}
								onClick={(e) => {
									e.stopPropagation();
									onToggleError();
								}}
							>
								<IcoX />
							</button>
						</div>
						<pre className="dl-pop-err">{draft.errorMessage ?? 'No error text recorded.'}</pre>
						<div className="dl-pop-meta">
							attempts {attempts} · last_attempt_at {formatClock(d.lastAttemptAt)} · claimed_at{' '}
							{formatClock(d.claimedAt)} · external_id {d.externalId ?? '—'}
						</div>
						<div className="dl-pop-actions">
							<button
								type="button"
								className="btn btn-sm btn-ghost"
								onClick={(e) => {
									e.stopPropagation();
									if (draft.errorMessage) void copyText(draft.errorMessage);
								}}
							>
								Copy error
							</button>
							{onRetry && (
								<button
									type="button"
									className="btn btn-sm"
									onClick={(e) => {
										e.stopPropagation();
										onRetry(draft.id);
										onToggleError();
									}}
								>
									<IcoRefresh />
									<span className="btn-label">Retry</span>
								</button>
							)}
						</div>
					</div>
				)}
			</span>
		);
	}

	return <span className={`dl-chip ${state}`}>{deliveryChipLabel(state, d)}</span>;
}

// Share mode: every field the approval is bound to, from the row the approval will carry (the
// same `seen` that travels with the commit). The header above shows only the recipient line the
// agent chose to display; the sender reads the real send list, cc, reply-to and threading from
// the payload, so they are listed here, routing fields first, nothing truncated.
function ShareBoundFields({ seen }: { seen: PaDraftSeen | undefined }) {
	const view = boundFields(seen);
	if (!view.ok) {
		return (
			<div className="ob-share-note" role="alert" aria-label="Approval covers">
				{view.reason}
			</div>
		);
	}
	const groups: Array<[string, typeof view.fields]> = [
		[
			'Channel and schedule',
			view.fields.filter((f) => f.group === 'channel' || f.group === 'schedule'),
		],
		['Draft content', view.fields.filter((f) => f.group === 'payload')],
		['Your edit', view.fields.filter((f) => f.group === 'edit')],
	];
	return (
		<section className="ob-share-bound" aria-label="Approval covers">
			<div className="ob-share-bound-title">
				Approving covers exactly these fields. The box sends from its own configured address.
			</div>
			{groups
				.filter(([, fields]) => fields.length > 0)
				.map(([title, fields]) => (
					<div key={title}>
						<div className="ob-share-bound-group">{title}</div>
						<dl className="ob-share-bound-list">
							{fields.map((f, i) => (
								// biome-ignore lint/suspicious/noArrayIndexKey: a static, read-only list rebuilt per row
								<div key={`${f.group}:${f.path}:${i}`} className="ob-share-bound-row">
									<dt>{f.path || '(value)'}</dt>
									<dd>{f.value}</dd>
								</div>
							))}
						</dl>
					</div>
				))}
		</section>
	);
}
