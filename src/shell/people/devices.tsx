// Devices tab — D-05 `devices` (`designs/people.html?state=devices`), WP-72.
//
// READ-ONLY, over what the daemon already exposes: is it running, where it
// listens (the tailnet address when it's bound to one), and the single bearer
// token. The design's toggles (Remote access on/off, Perimeter, Permission
// requests) become read-only values, because the shell has no API to change
// any of them. The pairing table shows only this device plus its empty state.
// There is no pairing: that is WP-74, behind G-ACCESS.
//
// The "not a security boundary" label is required. It stays while a single
// shared bearer token stands in for every client (Round 45 risk): anyone
// holding the token has the whole shell, and nothing can revoke one client
// without rotating the token for all of them.

import { RefreshCw, ShieldAlert } from 'lucide-react';
import { useCallback, useEffect, useState } from 'react';

import { Button } from '@/components/ui/button';
import { StatusChip } from '@/components/ui/status-chip';
import { type DaemonInfo, isRemoteWebSession, ptyDaemonInfo } from '@/lib/tauri-cmd';

import { useAppLockStore } from './app-lock-store';
import {
	buildDevicesView,
	type DevicesView,
	EXPOSURE_COPY,
	type ProbeResult,
	probeDaemon,
} from './devices-model';
import { Kv, PeopleBlock, PeopleHeader, PeopleRow } from './frame';

export function DevicesTab() {
	const { view, refreshing, refresh } = useDevicesView();
	return (
		<div data-state="devices" className="mx-auto w-full max-w-[960px] space-y-4 px-6 py-6">
			<PeopleHeader tab="devices" />
			<NotABoundary />
			<RemoteAccessBlock view={view} refreshing={refreshing} onRefresh={refresh} />
			<DevicesTableBlock view={view} />
		</div>
	);
}

function useDevicesView() {
	const [info, setInfo] = useState<DaemonInfo | null>(null);
	const [probe, setProbe] = useState<ProbeResult>('skipped');
	const [refreshing, setRefreshing] = useState(false);
	const remote = isRemoteWebSession();

	const refresh = useCallback(async () => {
		setRefreshing(true);
		try {
			if (remote) {
				setProbe(await probeDaemon(window.location.origin));
				return;
			}
			const next = await ptyDaemonInfo();
			setInfo(next);
			if (next?.available && next.mode === 'persistent' && next.httpUrl) {
				setProbe(await probeDaemon(next.httpUrl));
			} else {
				setProbe('skipped');
			}
		} finally {
			setRefreshing(false);
		}
	}, [remote]);

	useEffect(() => {
		void refresh();
	}, [refresh]);

	const view = buildDevicesView(
		remote ? null : info,
		remote
			? {
					origin: window.location.origin,
					hostname: window.location.hostname,
					hasToken: true,
				}
			: null,
		probe
	);
	return { view, refreshing, refresh };
}

function NotABoundary() {
	return (
		<div
			role="note"
			aria-label="Not a security boundary"
			className="flex items-start gap-3 rounded-[var(--radius-md,6px)] border border-[var(--border)] bg-[var(--bg-sunken)] px-3 py-2.5"
		>
			<ShieldAlert className="mt-0.5 h-4 w-4 shrink-0 text-[var(--warning)]" aria-hidden />
			<div className="min-w-0 space-y-1">
				<div className="flex flex-wrap items-center gap-2">
					<StatusChip tone="warn">Not a security boundary</StatusChip>
					<StatusChip tone="muted">read-only</StatusChip>
				</div>
				<p className="m-0 text-[var(--text-caption,12px)] leading-relaxed text-[var(--fg-muted)]">
					Remote access today is <b className="font-medium text-[var(--fg)]">one shared bearer
					token</b>. Every browser that holds it has the whole shell, and one client can't be
					revoked without rotating the token for all of them. Per-device pairing, capabilities and
					revocation come with G-ACCESS (Phase 7 Part B). Until then this view only shows what
					the daemon exposes.
				</p>
			</div>
		</div>
	);
}

const RUN_TONE = { running: 'live', stopped: 'muted', unknown: 'warn' } as const;

function RemoteAccessBlock({
	view,
	refreshing,
	onRefresh,
}: {
	view: DevicesView;
	refreshing: boolean;
	onRefresh: () => void;
}) {
	const exposure = EXPOSURE_COPY[view.exposure];
	const serving = view.run === 'running' && view.address !== null;
	return (
		<PeopleBlock
			title="Remote access"
			right={
				<>
					<Kv>daemon · {view.run}</Kv>
					<Button
						type="button"
						variant="ghost"
						size="icon-xs"
						aria-label="Check again"
						title="Check again"
						disabled={refreshing}
						onClick={onRefresh}
					>
						<RefreshCw className={refreshing ? 'animate-spin' : undefined} />
					</Button>
				</>
			}
		>
			<PeopleRow
				label="Reachable from elsewhere"
				sub="Serves this workspace over HTTP + WebSocket from the machine it runs on."
			>
				<StatusChip tone={RUN_TONE[view.run]} dot>
					{view.run}
				</StatusChip>
				<Kv className="break-all">{serving ? view.address : 'not serving'}</Kv>
				<Kv className="basis-full">{view.runNote}</Kv>
			</PeopleRow>
			<PeopleRow label="Perimeter" sub="Who can reach the address at all. Set where the daemon starts.">
				<StatusChip tone={exposure.tone}>{exposure.label}</StatusChip>
				<Kv className="basis-full">
					{view.address ? exposure.note : 'Nothing is listening, so nothing is exposed.'}
				</Kv>
			</PeopleRow>
			<PeopleRow label="Tailnet address">
				{view.tailnetAddress ? (
					<Kv className="text-[var(--fg)]">{view.tailnetAddress}</Kv>
				) : (
					<>
						<Kv>not on a tailnet</Kv>
						<Kv className="basis-full">
							To reach it from your other devices, run <code>ikenga-server --host</code> with this
							machine's tailnet address, or put it behind <code>tailscale serve</code>.
						</Kv>
					</>
				)}
			</PeopleRow>
			<PeopleRow
				label="Bearer token"
				sub="One token for every client. Whoever has it has everything."
			>
				{view.tokenPresent ? (
					<>
						<Kv className="text-[var(--fg)]">{view.tokenMasked}</Kv>
						<StatusChip tone="warn">shared · not per device</StatusChip>
					</>
				) : (
					<Kv>none issued</Kv>
				)}
			</PeopleRow>
			{view.source === 'desktop' && (
				<PeopleRow label="Process">
					<Kv>
						{view.mode === 'persistent' ? 'ikenga-server' : 'in-process (no daemon)'}
						{view.pid !== null && ` · pid ${view.pid}`}
					</Kv>
				</PeopleRow>
			)}
		</PeopleBlock>
	);
}

function DevicesTableBlock({ view }: { view: DevicesView }) {
	const status = useAppLockStore((s) => s.status);
	const thisDevice = status?.host || 'This device';
	return (
		<PeopleBlock title="Devices" right={<Kv>pairing arrives with G-ACCESS</Kv>}>
			<div className="-mx-3 overflow-x-auto">
				<table className="w-full border-collapse text-left text-[var(--text-caption,12px)]">
					<thead>
						<tr className="border-b border-[var(--border-soft)] text-[var(--text-micro)] uppercase tracking-[0.08em] text-[var(--fg-muted)]">
							<th className="px-3 py-2 font-semibold">Device</th>
							<th className="px-3 py-2 font-semibold">Last seen</th>
							<th className="px-3 py-2 font-semibold">Address</th>
							<th className="px-3 py-2 font-semibold">What it can do</th>
							<th className="px-3 py-2 text-right font-semibold">
								<span className="sr-only">Status</span>
							</th>
						</tr>
					</thead>
					<tbody>
						<tr className="border-b border-[var(--border-soft)]">
							<td className="px-3 py-2">
								<span className="block text-[var(--fg)]">{thisDevice}</span>
								<span className="block text-[var(--text-micro)] text-[var(--fg-muted)]">
									{status?.os || (view.source === 'remote' ? 'the host' : 'this desktop')}
								</span>
							</td>
							<td className="px-3 py-2 text-[var(--fg-muted)]">now</td>
							<td className="px-3 py-2 font-mono text-[var(--text-micro)] text-[var(--fg-muted)]">
								{view.host ?? 'local'}
							</td>
							<td className="px-3 py-2">
								<StatusChip tone="muted">Full</StatusChip>
							</td>
							<td className="px-3 py-2 text-right">
								<Kv>this device</Kv>
							</td>
						</tr>
						<tr>
							<td colSpan={5} className="px-3 py-4 text-[var(--text-caption,12px)] text-[var(--fg-muted)]">
								No paired devices. Nothing can pair yet. Any browser that has the token gets in
								without showing up here, and it can't be revoked on its own.
							</td>
						</tr>
					</tbody>
				</table>
			</div>
		</PeopleBlock>
	);
}
