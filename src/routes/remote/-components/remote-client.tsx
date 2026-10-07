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
import { useOnline } from '@/lib/pwa/use-online';
import {
	chiList,
	chiResume,
	notificationsList,
	ptyForeground,
	ptyForegroundSnapshot,
	ptyTerminalList,
	ptyWrite,
} from '@/lib/tauri-cmd';
import { D05_FOCUS } from '@/shell/people/focus';
import { closeResolvedNotifications } from '@/lib/pwa/deeplink';
import { InstallHint } from '@/shell/pwa/install-hint';
import { PushNudge } from '@/shell/pwa/push-nudge';
import { PushOpenNotice, usePushOpenHandling } from '@/shell/pwa/push-open';
import { PwaUpdateBanner } from '@/shell/pwa/pwa-update-banner';

import { RemoteInbox, SectionHead } from './inbox';
import {
	type AnnotatedRow,
	credentialLine,
	dispatchTargets,
	foregroundRefusal,
	NO_AGENT_TARGET,
	type SessionRow,
	sessionRows,
} from './remote-model';

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
			const [terms, runs, rows, foreground] = await Promise.all([
				ptyTerminalList().catch(() => []),
				chiList(null, 20).catch(() => []),
				notificationsList({ kinds: ['permission'], limit: 20 }).catch(() => []),
				ptyForegroundSnapshot().catch(() => ({})),
			]);
			setSessions(sessionRows(terms, runs, foreground));
			setAsks(rows as AnnotatedRow[]);
			// plans/pwa S4 §7: drop "Approval needed" notifications for asks
			// already answered (here or on another device).
			void closeResolvedNotifications(rows).catch(() => 0);
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
	const online = useOnline();
	const host = typeof window === 'undefined' ? '' : window.location.host;
	// A notification tap lands here on a phone; the inbox is the target.
	usePushOpenHandling();

	return (
		<div
			data-state="remote-client"
			className={`${D05_FOCUS} grid min-h-dvh justify-items-center bg-[var(--bg-base)] sm:py-6`}
		>
			<div className="flex min-h-dvh w-full max-w-[390px] flex-col overflow-hidden border-[var(--border-strong)] bg-[var(--bg-surface)] text-[var(--fg)] sm:min-h-0 sm:rounded-xl sm:border">
				<PwaUpdateBanner />
				<header className="flex items-start gap-2 px-3 py-2.5">
					<div className="min-w-0">
						<div className="truncate text-[length:var(--text-body-sm)] font-semibold">
							{status?.principal.username ?? 'Ikenga'}
						</div>
						<div className="truncate font-mono text-[length:var(--text-micro)] text-[var(--fg-muted)]">
							{host}
							{status ? ` · ${credentialLine(status)}` : ''}
						</div>
					</div>
					<span className="ml-auto">
						<StatusChip tone={connected ? 'live' : 'warn'} dot>
							{connected ? 'connected' : online ? 'reconnecting' : 'offline'}
						</StatusChip>
					</span>
				</header>

				<section data-section="sessions" aria-label="Sessions">
					<SectionHead title="Sessions" right={String(sessions.length)} />
					<ul className="m-0 list-none p-0">
						{sessions.length === 0 && (
							<li className="px-3 py-2 text-[length:var(--text-micro)] text-[var(--fg-muted)]">
								No sessions running.
							</li>
						)}
						{sessions.map((s) => (
							<li
								key={s.id}
								className="flex items-center gap-2 border-b border-[var(--border-soft)] px-3 py-2 font-mono text-[length:var(--text-micro)] last:border-b-0"
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
					<p className="m-0 px-3 py-2 text-[length:var(--text-micro)] leading-relaxed text-[var(--fg-muted)]">
						Tool calls stay on the computer for now; they reach paired devices with the ask relay.
					</p>
				</section>

				<div className="flex-1" />
				{error && (
					<p role="alert" className="m-0 px-3 py-1 text-[11px] text-[var(--danger)]">
						{connected || online
							? error
							: "Can't reach the Ikenga server — this device is offline."}
					</p>
				)}
				<PushOpenNotice className="border-t border-[var(--border-soft)] px-3 py-2" />
				<PushNudge className="border-t border-[var(--border-soft)] px-3 py-2.5" />
				<InstallHint className="border-t border-[var(--border-soft)] px-3 py-2" />
				{status && <DispatchBar status={status} sessions={sessions} />}
				{status && <ForgetDevice status={status} />}
			</div>
		</div>
	);
}

export function DispatchBar({
	status,
	sessions,
}: {
	status: AccessStatus;
	sessions: SessionRow[];
}) {
	const targets = useMemo(() => dispatchTargets(sessions), [sessions]);
	// No implicit default: the user picks a target for every send.
	const [target, setTarget] = useState<string>('');
	const [text, setText] = useState('');
	const [note, setNote] = useState<string | null>(null);
	const [busy, setBusy] = useState(false);
	const chosen = targets.find((t) => t.key === target);
	const blocked = disabledReason(chosen?.kind === 'chi' ? 'chi_resume' : 'pty_write', status);
	const pickBlocked =
		disabledReason('pty_write', status) !== null && disabledReason('chi_resume', status) !== null;
	const empty = targets.length === 0;

	const send = async (e: FormEvent) => {
		e.preventDefault();
		if (!chosen || !text.trim() || blocked || busy) return;
		setBusy(true);
		try {
			if (chosen.kind === 'pty') {
				// The foreground can change between picking and sending (the agent
				// exits back to its shell). Re-read it right before the write and
				// refuse unless an agent CLI is still in front.
				const refusal = foregroundRefusal(chosen, await ptyForeground(chosen.ptyId));
				if (refusal) {
					setTarget('');
					setNote(refusal);
					return;
				}
				await ptyWrite(chosen.ptyId, `${text}\r`);
			} else {
				const res = await chiResume(chosen.runId, text);
				if (res.status === 'failed' || res.error) {
					setNote(res.error ?? `Chi run ${res.status}`);
					return;
				}
			}
			setText('');
			setTarget('');
			setNote(`Sent to ${chosen.label}.`);
		} catch (err) {
			setNote(parseAccessError(err).message);
		} finally {
			setBusy(false);
		}
	};

	const hint = empty
		? NO_AGENT_TARGET
		: blocked
			? blocked
			: chosen
				? `↵ send to ${chosen.label}`
				: 'Choose an agent to send to';

	return (
		<form
			onSubmit={send}
			aria-label="Dispatch"
			data-dispatch={blocked || empty ? 'disabled' : 'enabled'}
			className="border-t border-[var(--border-soft)] px-3 py-2.5"
		>
			<select
				aria-label="Send to"
				value={chosen?.key ?? ''}
				onChange={(e) => {
					setTarget(e.target.value);
					setNote(null);
				}}
				disabled={empty || pickBlocked}
				className="mb-2 max-w-full rounded-full border border-[var(--border)] bg-[var(--bg-sunken)] px-2.5 py-1 font-mono text-[length:var(--text-micro)] text-[var(--fg)]"
			>
				<option value="">{empty ? 'no agent to send to' : 'Choose an agent…'}</option>
				{targets.map((t) => (
					<option key={t.key} value={t.key}>
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
					disabled={Boolean(blocked) || empty}
					className="min-w-0 flex-1 bg-transparent py-2 text-[length:var(--text-body-sm)] text-[var(--fg)] outline-none"
				/>
				<button
					type="submit"
					aria-label="Send to session"
					disabled={Boolean(blocked) || !text.trim() || !chosen || busy}
					className="text-[var(--fg-muted)] hover:text-[var(--fg)] disabled:opacity-40"
				>
					<Send className="h-4 w-4" />
				</button>
			</div>
			<p role="status" className="m-0 mt-1 text-[length:var(--text-micro)] text-[var(--fg-faint)]">
				{note ?? hint}
			</p>
		</form>
	);
}

function ForgetDevice({ status }: { status: AccessStatus }) {
	const [confirming, setConfirming] = useState(false);
	const id = status.credential.deviceId;
	if (status.credential.via !== 'device' || !id) return null;
	return (
		<div className="flex items-center gap-2 border-t border-[var(--border-soft)] px-3 py-2 text-[length:var(--text-micro)] text-[var(--fg-muted)]">
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
