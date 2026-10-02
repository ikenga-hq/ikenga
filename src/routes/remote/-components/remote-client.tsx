// `/remote` — the remote client (G-ACCESS §3.12, P-21; D-05
// `remote-client`). WP-74b.
//
// The 390 px Companion view a paired phone boots into when its grant is below
// `full`: Sessions, the Permission inbox, the Tool feed and the dispatch bar.
// No pane tree, no Explorer, no settings — a phone is for answering and
// instructing, not for editing. A `full` device or a password session gets
// the full shell and can still open `/remote` by hand.
//
// Every control that needs a cap this credential lacks is disabled with the
// reason (`disabledReason`, §1.5); the daemon or broker decides regardless.

import { Send } from 'lucide-react';
import { type FormEvent, useCallback, useEffect, useMemo, useState } from 'react';

import { StatusChip } from '@/components/ui/status-chip';
import {
	type AccessStatus,
	accessDeviceRevoke,
	accessStatus,
	disabledReason,
	parseAccessError,
} from '@/lib/access/client';
import { chiList, notificationsList, ptyTerminalList, ptyWrite } from '@/lib/tauri-cmd';

import { RemoteInbox, SectionHead } from './inbox';
import { type AnnotatedRow, credentialLine, type SessionRow, sessionRows } from './remote-model';

const POLL_MS = 3000;

function useRemoteData() {
	const [status, setStatus] = useState<AccessStatus | null>(null);
	const [sessions, setSessions] = useState<SessionRow[]>([]);
	const [asks, setAsks] = useState<AnnotatedRow[]>([]);
	const [error, setError] = useState<string | null>(null);
	const [connected, setConnected] = useState(true);

	const refresh = useCallback(async () => {
		try {
			const st = await accessStatus();
			setStatus(st);
			setConnected(true);
			const [terms, runs, rows] = await Promise.all([
				ptyTerminalList().catch(() => []),
				chiList(null, 20).catch(() => []),
				notificationsList({ kinds: ['permission'], limit: 20 }).catch(() => []),
			]);
			setSessions(sessionRows(terms, runs));
			setAsks(rows as AnnotatedRow[]);
			setError(null);
		} catch (e) {
			setConnected(false);
			setError(parseAccessError(e).message);
		}
	}, []);

	useEffect(() => {
		void refresh();
		const t = setInterval(() => void refresh(), POLL_MS);
		return () => clearInterval(t);
	}, [refresh]);

	return { status, sessions, asks, error, connected, refresh };
}

export function RemoteClient() {
	const { status, sessions, asks, error, connected, refresh } = useRemoteData();
	const host = typeof window === 'undefined' ? '' : window.location.host;

	return (
		<div
			data-state="remote-client"
			className="grid min-h-dvh justify-items-center bg-[var(--bg-base)] sm:py-6"
		>
			<div className="flex min-h-dvh w-full max-w-[390px] flex-col overflow-hidden border-[var(--border-strong)] bg-[var(--bg-surface)] text-[var(--fg)] sm:min-h-0 sm:rounded-xl sm:border">
				<header className="flex items-start gap-2 px-3 py-2.5">
					<div className="min-w-0">
						<div className="truncate text-[var(--text-body-sm)] font-semibold">
							{status?.principal.username ?? 'Ikenga'}
						</div>
						<div className="truncate font-mono text-[var(--text-micro)] text-[var(--fg-muted)]">
							{host}
							{status ? ` · ${credentialLine(status)}` : ''}
						</div>
					</div>
					<span className="ml-auto">
						<StatusChip tone={connected ? 'live' : 'warn'} dot>
							{connected ? 'connected' : 'reconnecting'}
						</StatusChip>
					</span>
				</header>

				<section data-section="sessions" aria-label="Sessions">
					<SectionHead title="Sessions" right={String(sessions.length)} />
					<ul className="m-0 list-none p-0">
						{sessions.length === 0 && (
							<li className="px-3 py-2 text-[var(--text-micro)] text-[var(--fg-muted)]">
								No sessions running.
							</li>
						)}
						{sessions.map((s) => (
							<li
								key={s.id}
								className="flex items-center gap-2 border-b border-[var(--border-soft)] px-3 py-2 font-mono text-[var(--text-micro)] last:border-b-0"
							>
								<span
									aria-hidden
									className={`h-1.5 w-1.5 flex-none rounded-full ${s.tone === 'live' ? 'bg-[var(--live)]' : s.tone === 'warn' ? 'bg-[var(--warning,var(--danger))]' : 'bg-[var(--fg-faint)]'}`}
								/>
								<span className="truncate text-[var(--fg)]">{s.label}</span>
								<span className="ml-auto flex-none text-[var(--fg-muted)]">{s.detail}</span>
							</li>
						))}
					</ul>
				</section>

				{status && <RemoteInbox rows={asks} status={status} onDecided={() => void refresh()} />}

				<section data-section="tool-feed" aria-label="Tool feed">
					<SectionHead title="Tool feed" />
					<p className="m-0 px-3 py-2 text-[var(--text-micro)] leading-relaxed text-[var(--fg-muted)]">
						Tool calls stay on the computer for now; they reach paired devices with the ask relay.
					</p>
				</section>

				<div className="flex-1" />
				{error && (
					<p role="alert" className="m-0 px-3 py-1 text-[11px] text-[var(--danger)]">
						{error}
					</p>
				)}
				{status && <DispatchBar status={status} sessions={sessions} />}
				{status && <ForgetDevice status={status} />}
			</div>
		</div>
	);
}

function DispatchBar({ status, sessions }: { status: AccessStatus; sessions: SessionRow[] }) {
	const targets = useMemo(() => sessions.filter((s) => s.ptyId), [sessions]);
	const [target, setTarget] = useState<string>('');
	const [text, setText] = useState('');
	const [note, setNote] = useState<string | null>(null);
	const blocked = disabledReason('pty_write', status);
	const chosen = targets.find((t) => t.ptyId === target) ?? targets[0];

	const send = async (e: FormEvent) => {
		e.preventDefault();
		if (!chosen?.ptyId || !text.trim() || blocked) return;
		try {
			await ptyWrite(chosen.ptyId, `${text}\r`);
			setText('');
			setNote('Sent.');
		} catch (err) {
			setNote(parseAccessError(err).message);
		}
	};

	return (
		<form
			onSubmit={send}
			aria-label="Dispatch"
			data-dispatch={blocked ? 'disabled' : 'enabled'}
			className="border-t border-[var(--border-soft)] px-3 py-2.5"
		>
			<select
				aria-label="Send to"
				value={chosen?.ptyId ?? ''}
				onChange={(e) => setTarget(e.target.value)}
				disabled={targets.length === 0 || Boolean(blocked)}
				className="mb-2 max-w-full rounded-full border border-[var(--border)] bg-[var(--bg-sunken)] px-2.5 py-1 font-mono text-[var(--text-micro)] text-[var(--fg)]"
			>
				{targets.length === 0 && <option value="">no terminal to send to</option>}
				{targets.map((t) => (
					<option key={t.id} value={t.ptyId ?? ''}>
						{t.label}
					</option>
				))}
			</select>
			<div className="flex items-center gap-2 rounded-md border border-[var(--border)] bg-[var(--bg-sunken)] px-2.5">
				<input
					aria-label="Dispatch an instruction"
					value={text}
					onChange={(e) => setText(e.target.value)}
					placeholder={blocked ?? 'Dispatch an instruction…'}
					disabled={Boolean(blocked) || targets.length === 0}
					className="min-w-0 flex-1 bg-transparent py-2 text-[var(--text-body-sm)] text-[var(--fg)] outline-none"
				/>
				<button
					type="submit"
					aria-label="Send to session"
					disabled={Boolean(blocked) || !text.trim() || !chosen}
					className="text-[var(--fg-muted)] hover:text-[var(--fg)] disabled:opacity-40"
				>
					<Send className="h-4 w-4" />
				</button>
			</div>
			<p className="m-0 mt-1 text-[var(--text-micro)] text-[var(--fg-faint)]">
				{note ?? (blocked ? blocked : '↵ send to session')}
			</p>
		</form>
	);
}

function ForgetDevice({ status }: { status: AccessStatus }) {
	const [confirming, setConfirming] = useState(false);
	const id = status.credential.deviceId;
	if (status.credential.via !== 'device' || !id) return null;
	return (
		<div className="flex items-center gap-2 border-t border-[var(--border-soft)] px-3 py-2 text-[var(--text-micro)] text-[var(--fg-muted)]">
			{confirming ? (
				<>
					<span>This device loses access at once.</span>
					<button
						type="button"
						className="ml-auto text-[var(--danger)]"
						onClick={async () => {
							try {
								await accessDeviceRevoke(id);
							} finally {
								window.location.assign('/remote/pair');
							}
						}}
					>
						Forget it
					</button>
					<button type="button" onClick={() => setConfirming(false)}>
						Keep
					</button>
				</>
			) : (
				<button type="button" className="ml-auto" onClick={() => setConfirming(true)}>
					Forget this device
				</button>
			)}
		</div>
	);
}
