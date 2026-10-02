// The remote client's permission inbox (G-ACCESS §3.12, §5.7; D-05
// `remote-client`). WP-74b ships the view; WP-75 fills its states: the
// desktop's asks reach a paired device through the T0 ask relay (§5.5 (a)),
// and the daemon's post-hook says per row whether THIS device may answer
// (`can_decide`), what it waits on (`waiting_on`) and whether "always for
// this project" is on offer (`can_allow_always`).
//
// A card is LIVE only when its row says `can_decide`; otherwise it renders
// read-only with the reason (D-7: a `dispatch` device can't approve; "this
// device only" names the device that can; a share's sensitive ask waits on
// the Owner).

import { useEffect, useState } from 'react';

import {
	type AccessStatus,
	accessRoutingGet,
	parseAccessError,
	permissionDecide,
} from '@/lib/access/client';
import { D05_DANGER } from '@/shell/people/focus';

import {
	decideErrorCopy,
	type InboxRow,
	inboxCardReason,
	needsRoutedDevice,
	offersAlways,
} from './inbox-model';

export function RemoteInbox({
	rows,
	status,
	onDecided,
}: {
	rows: InboxRow[];
	status: AccessStatus;
	onDecided: () => void;
}) {
	const pending = rows.filter((r) => !r.resolvedAt);
	const routedDevice = useRoutedDevice(needsRoutedDevice(pending));
	return (
		<section data-section="inbox" aria-label="Permission inbox">
			<SectionHead title="Permission inbox" right={`${pending.length} pending`} />
			<div className="flex flex-col gap-2 px-3 py-2">
				{pending.length === 0 && (
					<p className="m-0 py-2 text-[length:var(--text-micro)] text-[var(--fg-muted)]">
						Nothing is asking right now.
					</p>
				)}
				{pending.map((row) => (
					<InboxCard
						key={row.id}
						row={row}
						status={status}
						routedDevice={routedDevice}
						onDecided={onDecided}
					/>
				))}
			</div>
		</section>
	);
}

/** The device "this device only" names, fetched only when a card needs it. */
function useRoutedDevice(needed: boolean): string | null {
	const [name, setName] = useState<string | null>(null);
	useEffect(() => {
		if (!needed) return;
		let live = true;
		accessRoutingGet()
			.then((r) => {
				const n = (r as { deviceName?: string | null }).deviceName ?? null;
				if (live) setName(n);
			})
			.catch(() => {});
		return () => {
			live = false;
		};
	}, [needed]);
	return needed ? name : null;
}

function InboxCard({
	row,
	status,
	routedDevice,
	onDecided,
}: {
	row: InboxRow;
	status: AccessStatus;
	routedDevice: string | null;
	onDecided: () => void;
}) {
	const reason = inboxCardReason(row, status, routedDevice);
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const decide = async (decision: 'allow_once' | 'allow_always_project' | 'deny') => {
		setBusy(true);
		setError(null);
		try {
			await permissionDecide(row.id, decision);
			onDecided();
		} catch (e) {
			const { code, message } = parseAccessError(e);
			setError(decideErrorCopy(code, message));
			// The row changed under us (answered elsewhere, timed out,
			// re-routed): refresh so the card shows its new state.
			if (code && code !== 'internal') onDecided();
		} finally {
			setBusy(false);
		}
	};
	const btn =
		'rounded-md px-3 py-1.5 text-[length:var(--text-micro)] font-semibold disabled:opacity-50';
	return (
		<div
			data-card={reason ? 'read-only' : 'live'}
			data-waiting-on={row.waiting_on ?? undefined}
			className="rounded-md border border-[var(--border)] bg-[var(--bg-sunken)] px-3 py-2.5"
		>
			<p className="m-0 text-[length:var(--text-body-sm)] text-[var(--fg)]">{row.title}</p>
			{row.body && (
				<p className="m-0 mt-0.5 truncate font-mono text-[length:var(--text-micro)] text-[var(--fg-muted)]">
					{row.body}
				</p>
			)}
			{reason ? (
				<p className="m-0 mt-2 text-[length:var(--text-micro)] text-[var(--fg-muted)]">{reason}</p>
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
					{offersAlways(row) && (
						<button
							type="button"
							disabled={busy}
							onClick={() => decide('allow_always_project')}
							className={`${btn} text-[var(--fg-muted)] hover:text-[var(--fg)]`}
						>
							Always for this project
						</button>
					)}
					<button
						type="button"
						disabled={busy}
						onClick={() => decide('deny')}
						className={`${btn} ${D05_DANGER} border`}
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
		<h2 className="m-0 flex h-[30px] items-center border-y border-[var(--border-soft)] px-3 text-[length:var(--text-micro)] font-semibold uppercase tracking-[0.1em] text-[var(--fg-muted)]">
			{title}
			{right && (
				<span className="ml-auto font-mono font-normal normal-case tracking-normal">{right}</span>
			)}
		</h2>
	);
}
