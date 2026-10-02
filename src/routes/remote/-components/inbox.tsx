// The remote client's permission inbox (G-ACCESS §3.12, §5.7; D-05
// `remote-client`). WP-74b ships the view; WP-75 owns `inbox*` in W4
// (`can_decide` / `waiting_on` from its annotate, the T0 ask relay).
//
// A card is LIVE only when its row says `can_decide`; otherwise it renders
// read-only with the reason (D-7: a `dispatch` device can't approve).

import { useState } from 'react';

import { type AccessStatus, parseAccessError, permissionDecide } from '@/lib/access/client';

import { type AnnotatedRow, inboxReadOnlyReason } from './remote-model';

export function RemoteInbox({
	rows,
	status,
	onDecided,
}: {
	rows: AnnotatedRow[];
	status: AccessStatus;
	onDecided: () => void;
}) {
	const pending = rows.filter((r) => !r.resolvedAt);
	return (
		<section data-section="inbox" aria-label="Permission inbox">
			<SectionHead title="Permission inbox" right={`${pending.length} pending`} />
			<div className="flex flex-col gap-2 px-3 py-2">
				{pending.length === 0 && (
					<p className="m-0 py-2 text-[var(--text-micro)] text-[var(--fg-muted)]">
						Nothing is asking right now.
					</p>
				)}
				{pending.map((row) => (
					<InboxCard key={row.id} row={row} status={status} onDecided={onDecided} />
				))}
			</div>
		</section>
	);
}

function InboxCard({
	row,
	status,
	onDecided,
}: {
	row: AnnotatedRow;
	status: AccessStatus;
	onDecided: () => void;
}) {
	const reason = inboxReadOnlyReason(row, status);
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const decide = async (decision: 'allow_once' | 'allow_always_project' | 'deny') => {
		setBusy(true);
		setError(null);
		try {
			await permissionDecide(row.id, decision);
			onDecided();
		} catch (e) {
			setError(parseAccessError(e).message);
		} finally {
			setBusy(false);
		}
	};
	const btn = 'rounded-md px-3 py-1.5 text-[var(--text-micro)] font-semibold disabled:opacity-50';
	return (
		<div
			data-card={reason ? 'read-only' : 'live'}
			className="rounded-md border border-[var(--border)] bg-[var(--bg-sunken)] px-3 py-2.5"
		>
			<p className="m-0 text-[var(--text-body-sm)] text-[var(--fg)]">{row.title}</p>
			{row.body && (
				<p className="m-0 mt-0.5 truncate font-mono text-[var(--text-micro)] text-[var(--fg-muted)]">
					{row.body}
				</p>
			)}
			{reason ? (
				<p className="m-0 mt-2 text-[var(--text-micro)] text-[var(--fg-muted)]">{reason}</p>
			) : (
				<div className="mt-2 flex flex-wrap gap-2">
					<button
						type="button"
						disabled={busy}
						onClick={() => decide('allow_once')}
						className={`${btn} bg-[var(--primary)] text-[var(--primary-fg)]`}
					>
						Allow once
					</button>
					<button
						type="button"
						disabled={busy}
						onClick={() => decide('allow_always_project')}
						className={`${btn} text-[var(--fg-muted)] hover:text-[var(--fg)]`}
					>
						Always for this project
					</button>
					<button
						type="button"
						disabled={busy}
						onClick={() => decide('deny')}
						className={`${btn} border border-[var(--border)] text-[var(--danger)]`}
					>
						Deny
					</button>
				</div>
			)}
			{error && (
				<p role="alert" className="m-0 mt-1 text-[11px] text-[var(--danger)]">
					{error}
				</p>
			)}
		</div>
	);
}

export function SectionHead({ title, right }: { title: string; right?: string }) {
	return (
		<h2 className="m-0 flex h-[30px] items-center border-y border-[var(--border-soft)] px-3 text-[var(--text-micro)] font-semibold uppercase tracking-[0.1em] text-[var(--fg-muted)]">
			{title}
			{right && (
				<span className="ml-auto font-mono font-normal normal-case tracking-normal">{right}</span>
			)}
		</h2>
	);
}
