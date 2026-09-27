// WP-67 — the New-seat form, inline where you dispatch (D-09 `create` state;
// G-SEATS §1.2, §2.4 T1/T2a/T2b, §4.3, §6.1, §6a, §7.3, §9.3).
//
//   Name        validated live: §1.2 grammar, and "`<name>` is already a seat"
//   Engine      from `seats_engines()`; an engine that can't hold a seat is
//               disabled with its reason (D-09: gemini "not installed")
//   Start with  new session · resume a past session · an open session
//   Scratchpad  the seat's canonical scope, `seat:<project>/<name>` (§6a —
//               never the mockup's `seat:<name>` placeholder)
//   iyke        the exact CLI call the form makes (`--session` / `--resume`)
//
// *Seat this session…* opens it around a session with the engine locked.

import { useQuery } from '@tanstack/react-query';
import { X } from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { cn } from '@/components/ui/utils';
import { type SeatEngineInfo, type SeatView, seatsEngines } from '@/lib/tauri-cmd';
import { useShellStore } from '@/lib/shell/shell-store';
import { closeSeatForm, copyText, createSeat, type CreateSeatStart, selectSeat, type SeatFormInit } from './seat-actions';
import { checkSeatName, type CreateStart, engineShort, iykeSeatCreate, seatScope } from './seat-model';
import type { SeatRoster, UnseatedSession } from './seat-roster';
import { sessionName } from './seat-sessions';

type StartKind = 'new' | 'resume' | 'open';

/** Vacant seats whose last session this engine can resume — the form's
 *  *resume a past session* list (DEC-69c: creating moves it out of there). */
export function pastSessionsFor(seats: readonly SeatView[], engineId: string): SeatView[] {
	return seats.filter(
		(s) => s.status === 'vacant' && s.session !== null && s.resume.resumable && s.engine_id === engineId
	);
}

function RadioRow({
	on,
	disabled,
	title,
	onPick,
	sub,
	children,
	detail,
}: {
	on: boolean;
	disabled?: boolean;
	title?: string;
	onPick: () => void;
	sub?: boolean;
	children: React.ReactNode;
	detail?: React.ReactNode;
}) {
	return (
		<button
			type="button"
			role="radio"
			aria-checked={on}
			disabled={disabled}
			title={title}
			onClick={onPick}
			className={cn(
				'flex w-full items-start gap-2 border-b px-3 py-2 text-left last:border-b-0 enabled:hover:bg-[var(--bg-raised)] disabled:cursor-not-allowed',
				'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring',
				sub && 'pl-8'
			)}
			style={{ borderColor: 'var(--border-soft)' }}
		>
			<span
				aria-hidden="true"
				className="mt-[3px] size-3 shrink-0 rounded-full"
				style={
					on
						? { background: 'var(--primary)', boxShadow: 'inset 0 0 0 1.5px var(--primary), inset 0 0 0 4px var(--bg-surface)' }
						: { boxShadow: 'inset 0 0 0 1.5px var(--border-strong)' }
				}
			/>
			<span className="min-w-0">
				<span className="block text-[13px]" style={{ color: disabled ? 'var(--fg-muted)' : 'var(--fg)' }}>
					{children}
				</span>
				{detail && (
					<span className="block text-[11px]" style={{ color: 'var(--fg-muted)' }}>
						{detail}
					</span>
				)}
			</span>
		</button>
	);
}

export function SeatForm({ roster, init }: { roster: SeatRoster; init: SeatFormInit }) {
	const { seats, unseated, projectId } = roster;
	const defaultEngineId = useShellStore((s) => s.defaultEngineId);
	const engines = useQuery({ queryKey: ['seats', 'engines'], queryFn: seatsEngines, staleTime: 60_000 });
	const seated = init.seatSession ? unseated.find((u) => u.id === init.seatSession) : undefined;
	const lockedEngine = seated?.engineId ?? null;

	const engineList: SeatEngineInfo[] = useMemo(() => {
		if (engines.data?.length) return engines.data;
		// Until (or unless) the capability table loads, offer the default engine.
		const id = lockedEngine ?? defaultEngineId ?? 'claude-code';
		return [{ engine_id: id, wrap_id: null, engine_resume: null, seatable: true }];
	}, [engines.data, lockedEngine, defaultEngineId]);

	const firstSeatable =
		engineList.find((e) => e.engine_id === 'claude-code' && e.seatable) ?? engineList.find((e) => e.seatable);
	const [name, setName] = useState('');
	const [engine, setEngine] = useState<string>(lockedEngine ?? firstSeatable?.engine_id ?? 'claude-code');
	const [start, setStart] = useState<StartKind>(seated ? 'open' : 'new');
	const [openId, setOpenId] = useState<string | null>(seated?.id ?? null);
	const [resumeFrom, setResumeFrom] = useState<string | null>(null);
	const [error, setError] = useState<string | null>(null);
	const [busy, setBusy] = useState(false);
	const nameRef = useRef<HTMLInputElement | null>(null);

	// The capability table arrives after mount: land on its default engine.
	useEffect(() => {
		if (lockedEngine || !engines.data?.length) return;
		setEngine((cur) => {
			const ok = engines.data?.find((e) => e.engine_id === cur && e.seatable);
			if (ok) return cur;
			return (
				engines.data?.find((e) => e.engine_id === 'claude-code' && e.seatable)?.engine_id ??
				engines.data?.find((e) => e.seatable)?.engine_id ??
				cur
			);
		});
	}, [engines.data, lockedEngine]);

	useEffect(() => {
		nameRef.current?.focus();
	}, []);

	const check = checkSeatName(
		name,
		seats.map((s) => s.name),
		{ project: projectId }
	);
	const past = pastSessionsFor(seats, engine);
	const open: UnseatedSession[] = unseated.filter((u) => u.engineId === engine && u.status === 'running');
	const pastPick = past.find((p) => p.id === resumeFrom) ?? past[0];
	const openPick = open.find((u) => u.id === openId) ?? open[0];
	const info = engineList.find((e) => e.engine_id === engine);
	const hasWrap = Boolean(info?.wrap_id) || (!engines.data && engine === 'claude-code');

	const startOk =
		start === 'new' || (start === 'resume' && Boolean(pastPick)) || (start === 'open' && Boolean(openPick));
	const canCreate = check.ok && startOk && !busy && Boolean(info?.seatable ?? true);

	const iykeStart: CreateStart =
		start === 'open'
			? { kind: 'open', ref: openPick?.id ?? null }
			: start === 'resume'
				? {
						kind: 'resume',
						ref: pastPick?.session
							? pastPick.session.kind === 'terminal'
								? pastPick.session.terminal_id
								: pastPick.session.run_id
							: null,
					}
				: { kind: 'new' };
	const iyke = iykeSeatCreate(name, engine, iykeStart);
	const disabledReasons = engineList
		.filter((e) => !e.seatable)
		.map((e) => `${e.engine_id}: ${e.reason ?? 'can’t hold a seat'}`)
		.join(' · ');

	function pickEngine(id: string) {
		setEngine(id);
		setError(null);
		if (start === 'resume' && pastSessionsFor(seats, id).length === 0) setStart('new');
		if (start === 'open' && !unseated.some((u) => u.engineId === id && u.status === 'running')) {
			setStart('new');
			setOpenId(null);
		}
	}

	async function create() {
		if (!canCreate) {
			if (!check.ok) nameRef.current?.focus();
			return;
		}
		let createStart: CreateSeatStart = { kind: 'new' };
		if (start === 'resume' && pastPick) createStart = { kind: 'resume', from: pastPick };
		if (start === 'open' && openPick) {
			createStart = {
				kind: 'open',
				session: {
					kind: 'terminal',
					terminalId: openPick.id,
					engineId: engine,
					cwd: openPick.cwd,
					externalId: openPick.externalId,
				},
			};
		}
		setBusy(true);
		setError(null);
		try {
			const seat = await createSeat({ projectId, name: name.trim(), engineId: engine, hasWrap, start: createStart });
			closeSeatForm();
			selectSeat(seat);
		} catch (err) {
			setError(err instanceof Error ? err.message : String(err));
		} finally {
			setBusy(false);
		}
	}

	return (
		<section
			aria-label="New seat"
			data-state="seats-create"
			onKeyDown={(e) => {
				if (e.key === 'Escape') {
					e.preventDefault();
					e.stopPropagation();
					closeSeatForm();
				}
			}}
			className="flex min-h-0 flex-1 flex-col"
			style={{ background: 'var(--bg-surface)' }}
		>
			<div className="flex h-[30px] shrink-0 items-center gap-2 border-b pl-3 pr-2" style={{ borderColor: 'var(--border-soft)' }}>
				<span className="text-[11px] font-semibold uppercase tracking-[.1em]" style={{ color: 'var(--fg-muted)' }}>
					New seat
				</span>
				<button
					type="button"
					onClick={closeSeatForm}
					aria-label="Cancel (Esc)"
					title="Cancel (Esc)"
					className="ml-auto grid size-6 place-items-center rounded-sm text-[var(--fg-muted)] hover:bg-[var(--bg-raised)] hover:text-[var(--fg)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
				>
					<X className="h-3.5 w-3.5" aria-hidden="true" />
				</button>
			</div>

			<div className="min-h-0 flex-1 overflow-y-auto p-3">
				<div className="mb-3">
					<label htmlFor="seat-form-name" className="block text-[13px] font-medium" style={{ color: 'var(--fg)' }}>
						Name
					</label>
					<span className="block text-[11px]" style={{ color: 'var(--fg-muted)' }}>
						The address. a–z, 0–9 and -, unique in {projectId}.
					</span>
					<div
						className="mt-1 flex h-8 items-center gap-1 rounded-md border px-2 focus-within:border-[var(--primary)]"
						style={{
							background: 'var(--bg-sunken)',
							borderColor: !check.ok && !check.empty ? 'var(--danger)' : 'var(--border-strong)',
						}}
					>
						<span className="font-mono text-[13px]" style={{ color: 'var(--fg-muted)' }}>
							@
						</span>
						<input
							ref={nameRef}
							id="seat-form-name"
							type="text"
							value={name}
							data-seat-name-field=""
							autoComplete="off"
							spellCheck={false}
							aria-describedby="seat-form-name-msg"
							aria-invalid={(!check.ok && !check.empty) || undefined}
							onChange={(e) => {
								setName(e.target.value);
								setError(null);
							}}
							onKeyDown={(e) => {
								if (e.key === 'Enter') {
									e.preventDefault();
									void create();
								}
							}}
							className="min-w-0 flex-1 bg-transparent font-mono text-[13px] text-[var(--fg)] outline-none"
						/>
					</div>
					<span
						id="seat-form-name-msg"
						role="status"
						className="mt-1 block min-h-4 text-[11px]"
						style={{ color: check.ok ? 'var(--color-text-success)' : 'var(--color-text-danger)' }}
					>
						{check.message}
					</span>
				</div>

				<div className="mb-3">
					<span id="seat-form-engine" className="block text-[13px] font-medium" style={{ color: 'var(--fg)' }}>
						Engine
					</span>
					<div role="radiogroup" aria-labelledby="seat-form-engine" className="mt-1 flex flex-wrap gap-1">
						{engineList.map((e) => {
							const on = e.engine_id === engine;
							const locked = lockedEngine !== null && e.engine_id !== lockedEngine;
							return (
								<button
									key={e.engine_id}
									type="button"
									role="radio"
									aria-checked={on}
									disabled={!e.seatable || locked}
									title={
										!e.seatable
											? (e.reason ?? 'This engine can’t hold a seat')
											: locked && seated
												? `${sessionName(seated.id)} runs ${lockedEngine}`
												: ''
									}
									onClick={() => pickEngine(e.engine_id)}
									className={cn(
										'h-7 rounded-md border px-2 font-mono text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-60',
										on ? 'border-[var(--primary)] text-[var(--fg)]' : 'text-[var(--fg-muted)] enabled:hover:bg-[var(--bg-raised)]'
									)}
									style={on ? { background: 'var(--tint-bg-active)' } : { borderColor: 'var(--border)' }}
								>
									{e.engine_id}
								</button>
							);
						})}
					</div>
					<span className="mt-1 block text-[11px]" style={{ color: 'var(--fg-muted)' }}>
						{lockedEngine && seated
							? `Fixed by the session you are seating: ${sessionName(seated.id)} runs ${lockedEngine}.`
							: disabledReasons}
					</span>
				</div>

				<div className="mb-3">
					<span id="seat-form-start" className="block text-[13px] font-medium" style={{ color: 'var(--fg)' }}>
						Start with
					</span>
					<div
						role="radiogroup"
						aria-labelledby="seat-form-start"
						className="mt-2 overflow-hidden rounded-md border"
						style={{ borderColor: 'var(--border-soft)' }}
					>
						<RadioRow
							on={start === 'new'}
							onPick={() => setStart('new')}
							detail={
								hasWrap
									? `a fresh ${engine} session, started on Create`
									: `${engine} runs headless — the seat fills on its first dispatch`
							}
						>
							new session
						</RadioRow>
						<RadioRow
							on={start === 'resume'}
							disabled={past.length === 0}
							title={past.length ? '' : `No past ${engine} sessions`}
							onPick={() => setStart('resume')}
							detail={
								past.length
									? past.length === 1 && pastPick?.session
										? `${sessionName(pastPick.session.kind === 'terminal' ? pastPick.session.terminal_id : pastPick.session.run_id)} · last in @${pastPick.name}`
										: `${past.length} past sessions`
									: `no past ${engine} sessions`
							}
						>
							resume a past session
						</RadioRow>
						{start === 'resume' &&
							past.length > 1 &&
							past.map((p) => (
								<RadioRow
									key={p.id}
									sub
									on={pastPick?.id === p.id}
									onPick={() => setResumeFrom(p.id)}
									detail={`last in @${p.name}`}
								>
									<span className="font-mono">
										{p.session
											? sessionName(p.session.kind === 'terminal' ? p.session.terminal_id : p.session.run_id)
											: 'session'}
									</span>
								</RadioRow>
							))}
						<RadioRow
							on={start === 'open'}
							disabled={open.length === 0}
							title={open.length ? '' : `No unseated ${engine} sessions`}
							onPick={() => {
								setStart('open');
								if (!openId && open[0]) setOpenId(open[0].id);
							}}
							detail={
								open.length
									? `unseated: ${open.map((u) => sessionName(u.id)).join(', ')}`
									: `no unseated ${engine} sessions`
							}
						>
							an open session
						</RadioRow>
						{start === 'open' &&
							open.length > 1 &&
							open.map((u) => (
								<RadioRow
									key={u.id}
									sub
									on={openPick?.id === u.id}
									onPick={() => setOpenId(u.id)}
									detail={`${u.engineId ? engineShort(u.engineId) : 'shell'} · live`}
								>
									<span className="font-mono">{sessionName(u.id)}</span>
								</RadioRow>
							))}
					</div>
				</div>

				<div className="mb-1">
					<span className="block text-[13px] font-medium" style={{ color: 'var(--fg)' }}>
						Scratchpad
					</span>
					<div className="mt-2 flex flex-wrap items-center gap-2 text-[11px]" style={{ color: 'var(--fg-muted)' }}>
						<span className="font-mono text-[13px]" style={{ color: 'var(--fg)' }} data-seat-pad-preview="">
							{seatScope(projectId, name.trim() || '<name>')}
						</span>
						<span>created empty · shared through iyke scratchpad</span>
					</div>
				</div>
				{error && (
					<p role="alert" className="mt-2 text-[11px]" style={{ color: 'var(--color-text-danger)' }}>
						{error}
					</p>
				)}
			</div>

			<div
				className="flex min-h-7 shrink-0 items-center gap-2 border-t px-3 py-1 font-mono text-[11px]"
				style={{ borderColor: 'var(--border-soft)', color: 'var(--fg-muted)' }}
			>
				<b className="font-semibold" style={{ color: 'var(--fg)' }}>
					iyke
				</b>
				<span className="min-w-0 flex-1 break-all" style={{ color: 'var(--fg)' }} data-seat-form-iyke="">
					{iyke}
				</span>
				<button
					type="button"
					onClick={() => copyText(`iyke ${iyke}`, `Copied iyke ${iyke}`)}
					className="shrink-0 rounded-sm px-1 hover:text-[var(--fg)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
				>
					Copy
				</button>
			</div>
			<div className="flex shrink-0 items-center gap-2 p-3">
				<button
					type="button"
					disabled={!canCreate}
					title={check.ok ? (startOk ? `Create seat @${name.trim()}` : 'Choose what it starts with') : 'Name the seat first'}
					onClick={() => void create()}
					className="h-9 rounded-md bg-[var(--primary)] px-4 text-[13px] text-[var(--primary-fg)] hover:opacity-90 disabled:cursor-not-allowed disabled:bg-[var(--bg-raised)] disabled:text-[var(--fg-muted)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
				>
					{busy ? 'Creating…' : 'Create seat'}
				</button>
				<button
					type="button"
					onClick={closeSeatForm}
					className="h-9 rounded-md border px-4 text-[13px] text-[var(--fg)] hover:bg-[var(--bg-raised)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
					style={{ borderColor: 'var(--border)' }}
				>
					Cancel
				</button>
				<span className="text-[11px]" style={{ color: 'var(--fg-muted)' }}>
					Esc cancels
				</span>
			</div>
		</section>
	);
}
